//! The browser shell: the eframe web runner around [`SoundApp`].

use soundcraft_engine::Engine;
use soundcraft_ui_egui::{Services, SoundApp};
use std::sync::Arc;
use std::cell::RefCell;
use wasm_bindgen::JsCast as _;

thread_local! {
    /// Files pushed by the embedding host (Ardoise bridge `open`), imported on the next frame.
    static INBOX: RefCell<Vec<(String, Vec<u8>)>> = const { RefCell::new(Vec::new()) };
}

const CANVAS_ID: &str = "soundcraft_canvas";
const LOADING_ID: &str = "soundcraft_loading";

pub fn start() {
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    wasm_bindgen_futures::spawn_local(async {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            log::error!("no document");
            return;
        };
        let Some(canvas) = document.get_element_by_id(CANVAS_ID).and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok()) else {
            log::error!("missing <canvas id=\"{CANVAS_ID}\">");
            return;
        };
        let mut options = eframe::WebOptions::default();
        if query().contains("webgl")
            && let eframe::egui_wgpu::WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup
        {
            create.instance_descriptor.backends = eframe::wgpu::Backends::GL;
        }
        let result = eframe::WebRunner::new()
            .start(
                canvas,
                options,
                Box::new(move |cc| {
                    if let Some(rs) = &cc.wgpu_render_state {
                        log::info!("soundcraft-web: wgpu backend {:?}", rs.adapter.get_info().backend);
                    }
                    let ctx = cc.egui_ctx.clone();
                    crate::ardoise_bridge::install_bridge(move |name, bytes| {
                        let data = js_sys::Uint8Array::new(&bytes).to_vec();
                        INBOX.with(|i| i.borrow_mut().push((name, data)));
                        ctx.request_repaint();
                    });
                    let engine = if query().contains("empty") || crate::ardoise_bridge::active() { Engine::default() } else { soundcraft_engine::demo::demo_engine() };
                    let player = soundcraft_playback::Player::new(Arc::new(engine.session().clone()));
                    let app = SoundApp::new(engine, Some(player), Services::default());
                    Ok(Box::new(WebShell(app)))
                }),
            )
            .await;
        if let Some(el) = document.get_element_by_id(LOADING_ID) {
            match result {
                Ok(()) => el.remove(),
                Err(e) => el.set_inner_html(&format!("<p>Échec du démarrage d’Ardoise Son : {e:?}</p><p>Un navigateur avec WebGPU ou WebGL2 est requis.</p>")),
            }
        }
    });
}

fn query() -> String {
    web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default()
}

struct WebShell(SoundApp);

impl eframe::App for WebShell {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Files from the Ardoise host (bridge `open`) and files dropped on the page.
        let dropped: Vec<web_sys::File> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.web_file().cloned()).collect());
        for file in dropped {
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Ok(buf) = wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                    let data = js_sys::Uint8Array::new(&buf).to_vec();
                    INBOX.with(|i| i.borrow_mut().push((file.name(), data)));
                    ctx.request_repaint();
                }
            });
        }
        let files = INBOX.with(|i| std::mem::take(&mut *i.borrow_mut()));
        for (name, bytes) in files {
            match import_bytes(&mut self.0, &name, bytes) {
                Ok(()) => self.0.ui.status = format!("Imported {name}"),
                Err(e) => {
                    log::warn!("import {name}: {e}");
                    self.0.ui.status = e;
                }
            }
            ctx.request_repaint();
        }
        self.0.logic(ctx);
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.0.raw_input_hook(raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
        // Ardoise: offline mixdown to a single WAV, posted to the host (or downloaded standalone).
        let mut clicked = false;
        egui::Area::new(egui::Id::new("ardoise-export-mix")).anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 30.0)).show(ui.ctx(), |ui| {
            clicked = ui.button("Export mix (WAV)").clicked();
        });
        if clicked {
            match export_mix(&self.0) {
                Ok(()) => self.0.ui.status = "Mix exported".into(),
                Err(e) => {
                    log::warn!("export mix: {e}");
                    self.0.ui.status = e;
                }
            }
        }
    }
}

/// Import a file as audio, MIDI or video (one undo step), like the desktop Import commands.
fn import_bytes(app: &mut SoundApp, name: &str, bytes: Vec<u8>) -> Result<(), String> {
    use soundcraft_engine::io;
    let ext = name.rsplit('.').next().map(str::to_ascii_lowercase).unwrap_or_default();
    let engine = &mut app.engine;
    let before = engine.session_arc();
    let result = match ext.as_str() {
        "mid" | "midi" => io::import_midi_bytes(engine, &bytes, 0, true).map(|_| ()),
        "mp4" | "m4v" | "mov" => io::import_video_bytes(engine, name, bytes, 0).map(|_| ()),
        _ => io::import_audio_bytes(engine, name, &bytes, None, None, 0).map(|_| ()),
    };
    result.map_err(|e| e.to_string())?;
    engine.push_undo("Import", before);
    Ok(())
}

/// Render the session offline to one WAV file and hand it to the host (bridge v2, `export: true`);
/// outside an iframe it is downloaded.
fn export_mix(app: &SoundApp) -> Result<(), String> {
    use soundcraft_audio_io::{BitDepth, EncodeOptions};
    let e = &app.engine;
    let end = e.session().content_end().max(1);
    let format = soundcraft_audio_io::encode_format_for("wav").ok_or("no WAV encoder")?;
    let opts = EncodeOptions { format, bit_depth: BitDepth::Int24, dither: true, bwf: None };
    let range = soundcraft_time::Range::new(0, end);
    let (bytes, _) = soundcraft_engine::io::bounce_bytes_with(e, range, &opts, false, false).map_err(|err| err.to_string())?;
    let name = format!("{}.wav", soundcraft_engine::io::sanitize_name(&e.session().name));
    if crate::ardoise_bridge::post_saved(&name, &bytes) {
        return Ok(());
    }
    download(&name, &bytes)
}

fn download(name: &str, bytes: &[u8]) -> Result<(), String> {
    let js = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let document = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let bag = web_sys::BlobPropertyBag::new();
    bag.set_type("audio/wav");
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &bag).map_err(js)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(js)?;
    let a: web_sys::HtmlAnchorElement = document.create_element("a").map_err(js)?.dyn_into().map_err(|_| "not an anchor")?;
    a.set_href(&url);
    a.set_download(name);
    a.click();
    let _ = web_sys::Url::revoke_object_url(&url);
    Ok(())
}
