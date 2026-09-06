//! Presentation-only WASM client. Scientific computation and SRTM access are
//! intentionally absent from this crate.
pub fn configured_api_url() -> String {
    option_env!("RADAR_API_URL")
        .unwrap_or("http://localhost:8100")
        .to_owned()
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use wasm_bindgen::{prelude::*, JsCast};
    use wasm_bindgen_futures::{spawn_local, JsFuture};
    use web_sys::{HtmlMetaElement, Response};
    #[wasm_bindgen(start)]
    pub fn start() -> Result<(), JsValue> {
        let document = web_sys::window()
            .ok_or("window")?
            .document()
            .ok_or("document")?;
        let runtime = document
            .query_selector("meta[name='radar-api-url']")?
            .and_then(|e| e.dyn_into::<HtmlMetaElement>().ok())
            .map(|m| m.content())
            .filter(|v| !v.is_empty());
        let api = runtime.unwrap_or_else(super::configured_api_url);
        if let Some(e) = document.get_element_by_id("api-origin") {
            e.set_text_content(Some(&api));
        }
        let api_for_load = api.clone();
        let api_for_refresh = api.clone();
        spawn_local(async move {
            let status = check_health(&api)
                .await
                .unwrap_or_else(|_| "Indisponible".into());
            if let Some(e) = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.get_element_by_id("api-status"))
            {
                e.set_text_content(Some(&status));
            }
            if status == "Connecté" {
                if let Err(error) = load_radars(&api_for_load).await {
                    show_error(&format!("Chargement impossible: {error:?}"));
                }
            }
        });
        if let Some(button) = document.get_element_by_id("refresh") {
            let api = api_for_refresh;
            let callback = Closure::<dyn Fn()>::new(move || {
                let api = api.clone();
                spawn_local(async move {
                    if let Err(error) = load_radars(&api).await {
                        show_error(&format!("Chargement impossible: {error:?}"));
                    }
                })
            });
            button.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
            callback.forget();
        }
        Ok(())
    }
    async fn check_health(api: &str) -> Result<String, JsValue> {
        let url = format!("{}/health", api.trim_end_matches('/'));
        let response = JsFuture::from(web_sys::window().ok_or("window")?.fetch_with_str(&url))
            .await?
            .dyn_into::<Response>()?;
        Ok(if response.ok() {
            "Connecté".into()
        } else {
            format!("HTTP {}", response.status())
        })
    }
    async fn load_radars(api: &str) -> Result<(), JsValue> {
        let url = format!("{}/api/v1/radars", api.trim_end_matches('/'));
        let response = JsFuture::from(web_sys::window().ok_or("window")?.fetch_with_str(&url))
            .await?
            .dyn_into::<Response>()?;
        if !response.ok() {
            return Err(format!("HTTP {}", response.status()).into());
        }
        let text = JsFuture::from(response.text()?)
            .await?
            .as_string()
            .ok_or("response text")?;
        let radars: Vec<radar_api::Radar> =
            serde_json::from_str(&text).map_err(|e| JsValue::from_str(&e.to_string()))?;
        render_radars(&radars)
    }
    fn render_radars(radars: &[radar_api::Radar]) -> Result<(), JsValue> {
        let document = web_sys::window()
            .ok_or("window")?
            .document()
            .ok_or("document")?;
        let list = document
            .get_element_by_id("radar-list")
            .ok_or("radar-list")?;
        list.set_text_content(None);
        if radars.is_empty() {
            let empty = document.create_element("div")?;
            empty.set_class_name("card meta");
            empty.set_text_content(Some("Aucun radar configuré"));
            list.append_child(&empty)?;
            return Ok(());
        }
        for radar in radars {
            let card = document.create_element("div")?;
            card.set_class_name("card");
            let name = document.create_element("div")?;
            name.set_class_name("radar-name");
            name.set_text_content(Some(&format!(
                "{}{}",
                if radar.active { "● " } else { "○ " },
                radar.name
            )));
            let meta = document.create_element("div")?;
            meta.set_class_name("meta");
            meta.set_text_content(Some(&format!(
                "{:.4}°, {:.4}° · {:.0} km · antenne {:.0} m",
                radar.latitude,
                radar.longitude,
                radar.range_m / 1000.,
                radar.antenna_agl_m
            )));
            card.append_child(&name)?;
            card.append_child(&meta)?;
            list.append_child(&card)?;
        }
        show_error("");
        Ok(())
    }
    fn show_error(message: &str) {
        if let Some(e) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("ui-error"))
        {
            e.set_text_content(Some(message));
        }
    }
}
