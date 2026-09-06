#!/usr/bin/env bash
set -euo pipefail

API_URL="${RADIAL_API_URL:-http://127.0.0.1:8100}"
OUTPUT_DIR="${RADIAL_VALIDATION_DIR:-validation-output}"
TARGET_AGL_M="${RADIAL_TARGET_AGL_M:-75}"
LATITUDE="${RADIAL_LATITUDE:-45.2}"
LONGITUDE="${RADIAL_LONGITUDE:-2.2}"
RANGE_M="${RADIAL_RANGE_M:-20000}"
RESOLUTION_M="${RADIAL_RESOLUTION_M:-180}"
TIMEOUT_SECONDS="${RADIAL_TIMEOUT_SECONDS:-300}"
TILE_CONCURRENCY="${RADIAL_TILE_CONCURRENCY:-8}"
RADAR_ID="${RADIAL_RADAR_ID:-11111111-1111-4111-8111-111111111111}"

for command in curl jq od; do command -v "$command" >/dev/null || { echo "Commande requise absente: $command" >&2; exit 2; }; done
[[ "$TILE_CONCURRENCY" =~ ^[1-9][0-9]*$ ]] || { echo "RADIAL_TILE_CONCURRENCY doit être un entier positif" >&2; exit 2; }
mkdir -p "$OUTPUT_DIR"

post_json() {
  local url=$1 payload=$2 destination=$3 status
  status=$(curl -sS -o "$destination" -w '%{http_code}' -H 'content-type: application/json' -d "$payload" "$url") || {
    echo "Échec réseau lors de POST $url" >&2
    return 1
  }
  if (( status < 200 || status >= 300 )); then
    echo "POST $url a répondu HTTP $status:" >&2
    jq . "$destination" >&2 2>/dev/null || sed -n '1,20p' "$destination" >&2
    return 1
  fi
}

echo "[1/7] Attente du serveur $API_URL"
for _ in $(seq 1 30); do curl -fsS "$API_URL/health" >/dev/null && break; sleep 1; done
curl -fsS "$API_URL/ready" >/dev/null

radar_json=$(jq -n --arg id "$RADAR_ID" --argjson lat "$LATITUDE" --argjson lon "$LONGITUDE" --argjson range "$RANGE_M" '{id:$id,name:"Validation SRTM",latitude:$lat,longitude:$lon,antenna_agl_m:20,range_m:$range,active:true}')
echo "[2/7] Création/mise à jour du radar"
post_json "$API_URL/api/v1/radars" "$radar_json" "$OUTPUT_DIR/radar.json"

job_json=$(jq -n --argjson radar "$radar_json" --argjson resolution "$RESOLUTION_M" '{radars:[$radar],resolution_m:$resolution,effective_earth_k:1.3333333333333333,target_heights_agl_m:[30,50,100]}')
echo "[3/7] Job SRTM + LOS"
post_json "$API_URL/api/v1/jobs" "$job_json" "$OUTPUT_DIR/job-created.json"
job_id=$(jq -er '.id' "$OUTPUT_DIR/job-created.json")
deadline=$((SECONDS + TIMEOUT_SECONDS))
while (( SECONDS < deadline )); do
  curl -fsS "$API_URL/api/v1/jobs/$job_id" >"$OUTPUT_DIR/job-final.json"
  state=$(jq -er '.state' "$OUTPUT_DIR/job-final.json")
  case "$state" in completed) break;; failed|cancelled) jq . "$OUTPUT_DIR/job-final.json"; exit 1;; esac
  sleep 2
done
[[ "${state:-}" == completed ]] || { echo "Timeout du job $job_id" >&2; exit 1; }

echo "[4/7] Fusion à ${TARGET_AGL_M} m AGL"
fusion_json=$(jq -n --arg id "$RADAR_ID" --argjson height "$TARGET_AGL_M" '{radar_ids:[$id],target_agl_m:$height}')
post_json "$API_URL/api/v1/fusions" "$fusion_json" "$OUTPUT_DIR/fusion.json"
dataset_id=$(jq -er '.fusion_id' "$OUTPUT_DIR/fusion.json")
metadata_path=$(jq -er '.dataset_url' "$OUTPUT_DIR/fusion.json")
date=$(awk -F/ '{print $(NF-1)}' <<<"$metadata_path")
base="$API_URL/wmts/$dataset_id/1/$date"

echo "[5/7] Métadonnées et GetCapabilities"
curl -fsS "$base/metadata.json" -o "$OUTPUT_DIR/metadata.json"
curl -fsS "$base/WMTSCapabilities.xml" -o "$OUTPUT_DIR/WMTSCapabilities.xml"
grep -q '<Capabilities' "$OUTPUT_DIR/WMTSCapabilities.xml"
width=$(jq -er '.metadata.width' "$OUTPUT_DIR/metadata.json")
height=$(jq -er '.metadata.height' "$OUTPUT_DIR/metadata.json")
radar_col=$(jq -er '((0 - .metadata.extent[0]) / .metadata.resolution_m + 0.5) | floor' "$OUTPUT_DIR/metadata.json")
radar_row=$(jq -er '((.metadata.extent[3] - 0) / .metadata.resolution_m + 0.5) | floor' "$OUTPUT_DIR/metadata.json")

echo "[6/7] Téléchargement de la pyramide PNG complète"
layers=()
for candidate in ground agl-30m agl-50m agl-100m "agl-${TARGET_AGL_M}m" min-detection-height radar-count; do
  [[ " ${layers[*]:-} " == *" $candidate "* ]] || layers+=("$candidate")
done
max_size=$width
(( height > max_size )) && max_size=$height
levels=1
while (( max_size > 256 )); do max_size=$(( (max_size + 1) / 2 )); levels=$((levels + 1)); done
: >"$OUTPUT_DIR/tile-matrices.tsv"
tile_count=0
matrix_specs=""
pids=""
active_downloads=0
download_tile() {
  local layer=$1 z=$2 row=$3 col=$4 destination=$5
  curl -fsS "$base/$layer/$z/$row/$col.png" -o "$destination"
  local signature
  signature=$(od -An -tx1 -N8 "$destination" | tr -d ' \n')
  [[ "$signature" == 89504e470d0a1a0a ]]
}
for ((z=0; z<levels; z++)); do
  factor=$((1 << (levels - 1 - z)))
  span=$((256 * factor))
  matrix_width=$(( (width + span - 1) / span ))
  matrix_height=$(( (height + span - 1) / span ))
  matrix_specs="${matrix_specs}[$z,$factor,$matrix_width,$matrix_height],"
  printf '%s\t%s\t%s\t%s\n' "$z" "$factor" "$matrix_width" "$matrix_height" >>"$OUTPUT_DIR/tile-matrices.tsv"
  echo "      LOD $z: ${matrix_width}x${matrix_height} tuiles, facteur $factor"
  for layer in "${layers[@]}"; do
    directory="$OUTPUT_DIR/tiles/$layer/$z"
    mkdir -p "$directory"
    for ((row=0; row<matrix_height; row++)); do
      for ((col=0; col<matrix_width; col++)); do
        download_tile "$layer" "$z" "$row" "$col" "$directory/${row}-${col}.png" &
        pids="$pids $!"
        active_downloads=$((active_downloads + 1))
        tile_count=$((tile_count + 1))
        if (( active_downloads >= TILE_CONCURRENCY )); then
          for pid in $pids; do wait "$pid"; done
          pids=""
          active_downloads=0
        fi
      done
    done
  done
done
for pid in $pids; do wait "$pid"; done
for layer in "${layers[@]}"; do
  curl -fsS -D "$OUTPUT_DIR/${layer}.headers" "$base/$layer/0/0/0.png" -o "$OUTPUT_DIR/${layer}.png"
done

cat >"$OUTPUT_DIR/preview.html" <<HTML
<!doctype html><html lang="fr"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Radial · pyramide WMTS</title>
<style>*{box-sizing:border-box}html,body{height:100%;margin:0}body{display:grid;grid-template-rows:auto 1fr;background:#071116;color:#e9f1f4;font:14px system-ui}.toolbar{display:flex;align-items:center;gap:10px;flex-wrap:wrap;padding:10px 14px;background:#10232b;border-bottom:1px solid #29444e}.brand{font-weight:750;color:#9fe8d7;margin-right:8px}label{display:flex;align-items:center;gap:6px}select,button{color:#e9f1f4;background:#18313a;border:1px solid #42606b;border-radius:6px;padding:6px 9px}button{cursor:pointer}.status{margin-left:auto;color:#9bb4bd}main{position:relative;min-height:0;overflow:hidden}canvas{display:block;width:100%;height:100%;background:#08151a;cursor:grab;touch-action:none;image-rendering:pixelated}canvas.dragging{cursor:grabbing}.help,.tooltip{position:absolute;padding:7px 10px;border-radius:6px;background:#071116dd;color:#d9e8ec;pointer-events:none}.help{left:12px;bottom:12px;color:#9bb4bd}.tooltip{display:none;border:1px solid #42606b;white-space:nowrap;transform:translate(12px,12px)}</style>
<body><header class="toolbar"><span class="brand">RADIAL WMTS</span><label>Couche <select id="layer"></select></label><label>LOD <select id="lod"></select></label><button id="minus" title="Zoom arrière">−</button><button id="plus" title="Zoom avant">+</button><button id="fit">Ajuster</button><label><input id="grid" type="checkbox" checked> Grille des tuiles</label><strong>Radar : ${LATITUDE}°, ${LONGITUDE}°</strong><span class="status" id="status"></span></header><main id="viewport"><canvas id="map"></canvas><div class="help">Molette : zoom · Glisser : déplacer · [ / ] : changer de LOD · cercle rouge : radar</div><div class="tooltip" id="tooltip"></div></main>
<script>
const layers='${layers[*]}'.split(' '), matrices=[${matrix_specs%,}], sourceWidth=${width}, sourceHeight=${height}, sourceResolution=${RESOLUTION_M}, radarSourceCol=${radar_col}, radarSourceRow=${radar_row}, sampleUrl='${base}/sample', totalTiles=${tile_count};
const canvas=document.getElementById('map'),ctx=canvas.getContext('2d'),layerSelect=document.getElementById('layer'),lodSelect=document.getElementById('lod'),status=document.getElementById('status'),tooltip=document.getElementById('tooltip'),images=new Map(),samples=new Map();let sampleTimer=0,sampleController=null;
let lod=matrices.length-1,layer=layers[0],zoom=1,offsetX=0,offsetY=0,drag=null;
for(const name of layers)layerSelect.add(new Option(name,name));for(const matrix of matrices)lodSelect.add(new Option('LOD '+matrix[0]+' · '+matrix[2]+'×'+matrix[3]+' · facteur '+matrix[1],matrix[0]));lodSelect.value=lod;
function matrix(){return matrices[lod]}function resize(){const dpr=devicePixelRatio||1,w=canvas.clientWidth,h=canvas.clientHeight;if(canvas.width!==Math.round(w*dpr)||canvas.height!==Math.round(h*dpr)){canvas.width=Math.round(w*dpr);canvas.height=Math.round(h*dpr)}draw()}
function fit(){const m=matrix(),w=m[2]*256,h=m[3]*256;zoom=Math.min(canvas.clientWidth/w,canvas.clientHeight/h)*.96;offsetX=(canvas.clientWidth-w*zoom)/2;offsetY=(canvas.clientHeight-h*zoom)/2;draw()}
function tileImage(row,col){const key=layer+'/'+lod+'/'+row+'-'+col;if(!images.has(key)){const image=new Image();image.onload=draw;image.src='tiles/'+key+'.png';images.set(key,image)}return images.get(key)}
function draw(){const dpr=devicePixelRatio||1,m=matrix(),cw=canvas.clientWidth,ch=canvas.clientHeight;ctx.setTransform(dpr,0,0,dpr,0,0);ctx.clearRect(0,0,cw,ch);const firstCol=Math.max(0,Math.floor(-offsetX/(256*zoom))),lastCol=Math.min(m[2]-1,Math.floor((cw-offsetX)/(256*zoom))),firstRow=Math.max(0,Math.floor(-offsetY/(256*zoom))),lastRow=Math.min(m[3]-1,Math.floor((ch-offsetY)/(256*zoom)));ctx.imageSmoothingEnabled=false;ctx.setTransform(dpr*zoom,0,0,dpr*zoom,dpr*offsetX,dpr*offsetY);for(let row=firstRow;row<=lastRow;row++)for(let col=firstCol;col<=lastCol;col++){const image=tileImage(row,col);if(image.complete&&image.naturalWidth)ctx.drawImage(image,col*256,row*256);if(document.getElementById('grid').checked){ctx.strokeStyle='#45d4b8aa';ctx.lineWidth=1/zoom;ctx.strokeRect(col*256,row*256,256,256)}}const radarX=radarSourceCol/m[1],radarY=radarSourceRow/m[1],marker=9/zoom;ctx.strokeStyle='#ff3b30';ctx.lineWidth=2/zoom;ctx.beginPath();ctx.arc(radarX,radarY,marker,0,Math.PI*2);ctx.stroke();ctx.beginPath();ctx.moveTo(radarX-marker*1.5,radarY);ctx.lineTo(radarX+marker*1.5,radarY);ctx.moveTo(radarX,radarY-marker*1.5);ctx.lineTo(radarX,radarY+marker*1.5);ctx.stroke();ctx.setTransform(dpr,0,0,dpr,0,0);status.textContent=sourceWidth+'×'+sourceHeight+' · '+matrices.length+' LOD · '+totalTiles+' PNG · zoom '+Math.round(zoom*100)+'%'}
function zoomAt(factor,x,y){const next=Math.max(.01,Math.min(64,zoom*factor));offsetX=x-(x-offsetX)*next/zoom;offsetY=y-(y-offsetY)*next/zoom;zoom=next;draw()}
function sampleText(value){const terrain=value.terrain_elevation_amsl_m===null?'NoData':value.terrain_elevation_amsl_m+' m AMSL',minimum=value.minimum_detection_agl_m===null?'NoData':value.minimum_detection_agl_m+' m AGL';return ' · Terrain '+terrain+' · Détection min. '+minimum}
function updateTooltip(event){const rect=canvas.getBoundingClientRect(),x=event.clientX-rect.left,y=event.clientY-rect.top,m=matrix(),sourceCol=(x-offsetX)/zoom*m[1],sourceRow=(y-offsetY)/zoom*m[1];if(sourceCol<0||sourceRow<0||sourceCol>=sourceWidth||sourceRow>=sourceHeight){tooltip.style.display='none';return}const col=Math.floor(sourceCol),row=Math.floor(sourceRow),key=col+','+row,east=(sourceCol-radarSourceCol)*sourceResolution,north=(radarSourceRow-sourceRow)*sourceResolution,distance=Math.hypot(east,north)/1000,bearing=(Math.atan2(east,north)*180/Math.PI+360)%360,baseText='Cap '+bearing.toFixed(1)+'° · '+distance.toFixed(2)+' km';tooltip.dataset.sampleKey=key;tooltip.textContent=baseText+(samples.has(key)?sampleText(samples.get(key)):' · altitudes…');tooltip.style.left=x+'px';tooltip.style.top=y+'px';tooltip.style.display='block';clearTimeout(sampleTimer);sampleTimer=setTimeout(()=>{if(sampleController)sampleController.abort();sampleController=new AbortController();fetch(sampleUrl+'?col='+col+'&row='+row,{signal:sampleController.signal}).then(response=>{if(!response.ok)throw new Error('sample');return response.json()}).then(value=>{samples.set(key,value);if(tooltip.dataset.sampleKey===key)tooltip.textContent=baseText+sampleText(value)}).catch(error=>{if(error.name!=='AbortError'&&tooltip.dataset.sampleKey===key)tooltip.textContent=baseText+' · altitudes indisponibles'})},80)}
canvas.addEventListener('wheel',event=>{event.preventDefault();const rect=canvas.getBoundingClientRect();zoomAt(Math.exp(-event.deltaY*.0015),event.clientX-rect.left,event.clientY-rect.top);updateTooltip(event)},{passive:false});canvas.addEventListener('pointerdown',event=>{canvas.setPointerCapture(event.pointerId);drag=[event.clientX,event.clientY,offsetX,offsetY];canvas.classList.add('dragging')});canvas.addEventListener('pointermove',event=>{if(drag){offsetX=drag[2]+event.clientX-drag[0];offsetY=drag[3]+event.clientY-drag[1];draw()}updateTooltip(event)});canvas.addEventListener('pointerleave',()=>{tooltip.style.display='none'});canvas.addEventListener('pointerup',()=>{drag=null;canvas.classList.remove('dragging')});canvas.addEventListener('pointercancel',()=>{drag=null;canvas.classList.remove('dragging');tooltip.style.display='none'});
layerSelect.onchange=()=>{layer=layerSelect.value;draw()};lodSelect.onchange=()=>{lod=Number(lodSelect.value);fit()};document.getElementById('grid').onchange=draw;document.getElementById('minus').onclick=()=>zoomAt(.5,canvas.clientWidth/2,canvas.clientHeight/2);document.getElementById('plus').onclick=()=>zoomAt(2,canvas.clientWidth/2,canvas.clientHeight/2);document.getElementById('fit').onclick=fit;addEventListener('keydown',event=>{if(event.key==='['&&lod>0){lod--;lodSelect.value=lod;fit()}if(event.key===']'&&lod<matrices.length-1){lod++;lodSelect.value=lod;fit()}});new ResizeObserver(resize).observe(document.getElementById('viewport'));resize();fit();
</script></body></html>
HTML

echo "[7/7] Validation ETag / 304"
etag=$(awk 'tolower($1)=="etag:"{gsub("\r","");print $2}' "$OUTPUT_DIR/radar-count.headers")
[[ -n "$etag" ]] || { echo "ETag absent" >&2; exit 1; }
code=$(curl -sS -o /dev/null -w '%{http_code}' -H "If-None-Match: $etag" "$base/radar-count/0/0/0.png")
[[ "$code" == 304 ]] || { echo "304 attendu, reçu $code" >&2; exit 1; }

echo "VALIDATION OK"
echo "Job: $job_id"
echo "Dataset: $dataset_id"
echo "Pyramide PNG: $tile_count tuiles, $levels LOD dans $OUTPUT_DIR/tiles"
echo "Matrices: $OUTPUT_DIR/tile-matrices.tsv"
echo "Aperçu par LOD: $OUTPUT_DIR/preview.html"
