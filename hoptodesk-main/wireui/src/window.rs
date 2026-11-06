use crate::capi::sctypes::{HWINDOW, RECT};
use crate::dom::event::EventHandler;
use crate::host::{Host, HostHandler};
use std::rc::Rc;

#[derive(Default)]
pub struct Builder {
    rect: RECT,
    main: bool,
}

impl Builder {
    pub fn main_window() -> Builder {
        Builder {
            rect: RECT::default(),
            main: true,
        }
    }

    pub fn main(mut self) -> Builder {
        self.main = true;
        self
    }

    pub fn with_rect(mut self, rect: RECT) -> Builder {
        self.rect = rect;
        self
    }

    pub fn create(self) -> Window {
        Window::create(self.rect, self.main)
    }
}

pub struct Window {
    host: Rc<Host>,
    title: String,
    handler: Option<crate::bridge::SharedHandler>,
    behaviors: Vec<(String, crate::engine::window::BehaviorFactory)>,
    page_uri: Option<String>,
    archive: Option<crate::engine::archive::Archive>,
}

impl Window {
    fn create(rect: RECT, main: bool) -> Window {
        let _ = (rect, main);
        Window {
            host: Rc::new(Host::new()),
            title: "HopToDesk".to_string(),
            handler: None,
            behaviors: Vec::new(),
            page_uri: None,
            archive: None,
        }
    }

    pub fn get_host(&self) -> Rc<Host> {
        Rc::clone(&self.host)
    }

    pub fn get_hwnd(&self) -> HWINDOW {
        self.host.get_hwnd()
    }

    pub fn set_title(&mut self, title: &str) {
        self.title = title.to_string();
    }

    pub fn event_handler<Handler: EventHandler + 'static>(&mut self, handler: Handler) {
        self.handler = Some(std::rc::Rc::new(std::cell::RefCell::new(
            Box::new(handler) as Box<dyn EventHandler>,
        )));
    }

    pub fn set_host_handler<Callback: HostHandler + Sized>(&mut self, handler: Callback) {
        let _ = handler;
    }

    pub fn archive_handler(&mut self, resource: &[u8]) -> Result<(), ()> {
        match crate::engine::archive::Archive::parse(resource) {
            Ok(a) => {
                self.archive = Some(a);
                Ok(())
            }
            Err(_) => Err(()),
        }
    }

    pub fn register_behavior<Factory>(&mut self, name: &str, factory: Factory)
    where
        Factory: Fn() -> Box<dyn EventHandler> + 'static,
    {
        let shared: crate::engine::window::BehaviorFactory = std::rc::Rc::new(move || {
            std::rc::Rc::new(std::cell::RefCell::new(factory()))
        });
        self.behaviors.push((name.to_string(), shared));
    }

    pub fn load_file(&mut self, uri: &str) -> bool {
        self.page_uri = Some(uri.to_string());
        true
    }

    pub fn load_html(&mut self, html: &[u8], uri: Option<&str>) -> bool {
        let _ = (html, uri);
        true
    }

    pub fn expand(&mut self) {}

    pub fn collapse(&mut self, hide: bool) {
        let _ = hide;
    }

    pub fn dismiss(&mut self) {}

    fn resolve_page(&self) -> Option<(std::path::PathBuf, Option<std::path::PathBuf>)> {
        let uri = self.page_uri.as_deref()?;
        let path = uri
            .strip_prefix("file://")
            .unwrap_or(uri)
            .split('?')
            .next()
            .unwrap_or(uri);
        let pb = std::path::PathBuf::from(path);
        if pb.exists() {
            let base = pb.parent().map(|p| p.to_path_buf());
            Some((pb, base))
        } else {
            None
        }
    }

    pub fn run_app(mut self) {
        let platform = if cfg!(target_os = "macos") {
            "OSX"
        } else if cfg!(target_os = "windows") {
            "Windows"
        } else {
            "Linux"
        };
        let archive_page = self
            .page_uri
            .as_deref()
            .and_then(|u| u.strip_prefix("this://app/"))
            .map(|p| p.split('?').next().unwrap_or(p).to_string());
        let source = match (archive_page, self.archive.take()) {
            (Some(page), Some(archive)) => {
                crate::engine::window::PageSource::Archive { archive, page }
            }
            (Some(page), None) => {
                eprintln!("wireui: this://app/{} requested but no archive registered", page);
                return;
            }
            _ => match self.resolve_page() {
                Some((page, base)) => crate::engine::window::PageSource::Path { page, base },
                None => {
                    eprintln!(
                        "wireui: no loadable page ({:?}); nothing to run",
                        self.page_uri
                    );
                    return;
                }
            },
        };
        if let Err(e) = crate::engine::window::run_window_source(
            source,
            platform,
            (800, 600),
            &self.title,
            None,
            self.handler,
            self.behaviors,
        ) {
            eprintln!("wireui window error: {}", e);
        }
    }

    pub fn run_loop(self) {
        self.run_app();
    }

    pub fn quit_app(&self) {}
}
