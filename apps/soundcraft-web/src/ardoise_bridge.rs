//! Ardoise host bridge (postMessage). Protocol: see BRIDGE.md in the Ardoise repository.
//!   -> parent  {type:"ready", app}                           once the editor is up
//!   <- editor  {type:"open", name, bytes:ArrayBuffer}         only from window.parent at ?parentOrigin=
//!   -> parent  {type:"saved", app, name, bytes:ArrayBuffer}   native-format save inside an iframe
//!
//! Without a valid `?parentOrigin=` query parameter (standalone page) the bridge is disabled:
//! no listener, no `ready`, and Save downloads as upstream.

const ARDOISE_APP: &str = "sound";
/// Extensions (lower case, no dot) whose Save / Save As is handed to the host instead of downloaded.
const NATIVE_EXTS: &[&str] = &[];

fn js_err(e: wasm_bindgen::JsValue) -> String {
    format!("{e:?}")
}

fn query() -> String {
    web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default()
}

/// True when this page is embedded in another window (an iframe).
fn in_iframe() -> bool {
    let Some(w) = web_sys::window() else {
        return false;
    };
    match w.parent() {
        Ok(Some(p)) => !js_sys::Object::is(&p, &w),
        _ => false,
    }
}

fn str_field(obj: &wasm_bindgen::JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(obj, &wasm_bindgen::JsValue::from_str(key)).ok()?.as_string()
}

/// Accept only `https://host[:port]` or `http://localhost[:port]` (no path, query, fragment or userinfo).
fn valid_origin(o: &str) -> bool {
    let (rest, https) = match (o.strip_prefix("https://"), o.strip_prefix("http://")) {
        (Some(r), _) => (r, true),
        (None, Some(r)) => (r, false),
        _ => return false,
    };
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':')) {
        return false;
    }
    let (host, port) = match rest.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (rest, None),
    };
    if host.is_empty() || host.starts_with(['.', '-']) || host.ends_with(['.', '-']) || host.contains(':') {
        return false;
    }
    if let Some(p) = port
        && (p.is_empty() || p.len() > 5 || !p.chars().all(|c| c.is_ascii_digit()))
    {
        return false;
    }
    https || host == "localhost"
}

/// The embedding page's origin, from `?parentOrigin=...`. `None` disables the bridge.
fn parent_origin() -> Option<String> {
    let q = query();
    let raw = q.trim_start_matches('?').split('&').find_map(|kv| kv.strip_prefix("parentOrigin="))?;
    let decoded: String = js_sys::decode_uri_component(raw).ok()?.into();
    valid_origin(&decoded).then_some(decoded)
}

/// Post `msg` to the parent window, addressed to the validated parent origin only (never "*").
fn post_to_parent(msg: &js_sys::Object) -> Result<(), String> {
    let origin = parent_origin().ok_or("bridge disabled (no valid parentOrigin)")?;
    let w = web_sys::window().ok_or("no window")?;
    let parent = w.parent().map_err(js_err)?.ok_or("no parent window")?;
    parent.post_message(msg, &origin).map_err(js_err)
}

fn message(fields: &[(&str, wasm_bindgen::JsValue)]) -> js_sys::Object {
    let o = js_sys::Object::new();
    for (k, v) in fields {
        let _ = js_sys::Reflect::set(&o, &wasm_bindgen::JsValue::from_str(k), v);
    }
    o
}

/// Listen for `open` messages from the parent window (strict origin + source checks) and announce
/// `ready`. `on_open(name, bytes)` receives the file name and the `ArrayBuffer` / `Uint8Array`.
pub fn install_bridge(on_open: impl Fn(String, wasm_bindgen::JsValue) + 'static) {
    use wasm_bindgen::JsCast as _;
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(parent_origin) = parent_origin() else {
        log::info!("bridge disabled: no valid parentOrigin in the query string");
        return;
    };
    let on_message = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |ev: web_sys::MessageEvent| {
        if ev.origin() != parent_origin {
            return;
        }
        let from_parent = match (ev.source(), web_sys::window().and_then(|w| w.parent().ok().flatten())) {
            (Some(src), Some(par)) => js_sys::Object::is(&src, &par),
            _ => false,
        };
        if !from_parent {
            return;
        }
        let data = ev.data();
        if str_field(&data, "type").as_deref() != Some("open") {
            return;
        }
        let Ok(bytes) = js_sys::Reflect::get(&data, &wasm_bindgen::JsValue::from_str("bytes")) else {
            return;
        };
        if !(bytes.is_instance_of::<js_sys::ArrayBuffer>() || bytes.is_instance_of::<js_sys::Uint8Array>()) {
            log::warn!("bridge: open message without ArrayBuffer bytes ignored");
            return;
        }
        let name = str_field(&data, "name").filter(|n| !n.is_empty()).unwrap_or_else(|| "document".into());
        on_open(name, bytes);
    });
    let _ = window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref());
    on_message.forget();
    if in_iframe()
        && let Err(e) = post_to_parent(&message(&[("type", "ready".into()), ("app", ARDOISE_APP.into())]))
    {
        log::error!("bridge: ready message failed: {e}");
    }
}

/// Native-format save inside an iframe: hand the bytes to the host instead of downloading.
/// Returns true when the bytes were posted (the caller then skips the browser download).
#[allow(dead_code)]
pub fn post_saved(name: &str, bytes: &[u8]) -> bool {
    let ext = name.rsplit('.').next().map(str::to_ascii_lowercase).unwrap_or_default();
    if !in_iframe() || parent_origin().is_none() || !NATIVE_EXTS.contains(&ext.as_str()) {
        return false;
    }
    let buffer = js_sys::Uint8Array::from(bytes).buffer();
    let msg = message(&[("type", "saved".into()), ("app", ARDOISE_APP.into()), ("name", name.into()), ("bytes", buffer.into())]);
    match post_to_parent(&msg) {
        Ok(()) => true,
        Err(e) => {
            log::error!("bridge: saved message failed ({e}); falling back to download");
            false
        }
    }
}
