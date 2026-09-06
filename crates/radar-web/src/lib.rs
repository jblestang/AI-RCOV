//! Presentation-only WASM client. Scientific computation and SRTM access are
//! intentionally absent from this crate.
pub fn configured_api_url() -> String {
    option_env!("RADAR_API_URL")
        .unwrap_or("http://localhost:8080")
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
        });
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
}
