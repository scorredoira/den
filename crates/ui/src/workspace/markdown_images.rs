//! A Markdown document's images: files from its machine (including over
//! SSH) and `data:` URLs. Its tabs share them and free them when the last one
//! goes; gpui's shared cache would keep them, decoded and on the GPU, until
//! the app quits. (Den has no HTTP client: web images never load.)
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use client::Client;
use data_url::DataUrl;
use gpui_kit::*;
use proto::{Request, Response};

use super::{image_format, normalize, percent_decode};

pub(super) fn resolver(
    client: Option<Arc<Client>>,
    root: PathBuf,
    document: PathBuf,
    images: Entity<PreviewImages>,
) -> impl Fn(&SharedUri) -> Option<ImageSource> + Send + Sync + 'static {
    move |url| {
        let source = match image_path(url.as_ref(), &root, &document) {
            Some(path) => Source::File(path),
            None if url.starts_with("data:") => Source::Data(url.clone()),
            None => return None,
        };
        let (client, images) = (client.clone(), images.clone());
        Some(ImageSource::Custom(Arc::new(move |window, cx| {
            images.update(cx, |images, cx| images.load(&source, client.as_ref(), window, cx))
        })))
    }
}

type Loaded = Result<Arc<RenderImage>, ImageCacheError>;

#[derive(Clone, PartialEq, Eq, Hash)]
enum Source {
    File(PathBuf),
    Data(SharedUri),
}

/// An image read and decoded by the task, which goes with it.
struct Load {
    loaded: Option<Loaded>,
    _task: Task<()>,
}

/// The images of one document's previews.
pub(super) struct PreviewImages(HashMap<Source, Load>);

impl PreviewImages {
    pub(super) fn new(cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| {
            for load in this.0.values() {
                if let Some(Ok(image)) = &load.loaded {
                    cx.drop_image(image.clone(), None);
                }
            }
        })
        .detach();
        Self(HashMap::new())
    }

    fn load(&mut self, source: &Source, client: Option<&Arc<Client>>, window: &mut Window, cx: &mut Context<Self>) -> Option<Loaded> {
        if !self.0.contains_key(source) {
            let load = match source {
                Source::File(path) => {
                    let read = client.map(|client| client.request(Request::ReadFile { path: path.clone() }));
                    let (format, path) = (image_format(path), path.clone());
                    self.spawn(source, window, cx, async move {
                        let format = format.ok_or_else(|| anyhow::anyhow!("Unsupported image: {}", path.display()))?;
                        match read.ok_or_else(|| anyhow::anyhow!("No agent for Markdown image"))?.await? {
                            Response::Bytes(bytes) => Ok(Image::from_bytes(format, bytes)),
                            other => Err(anyhow::anyhow!("Unexpected image response: {other:?}")),
                        }
                    })
                }
                Source::Data(url) => {
                    let url = url.clone();
                    self.spawn(source, window, cx, async move {
                        data_url_image(&url).ok_or_else(|| anyhow::anyhow!("Unsupported image: {url}"))
                    })
                }
            };
            self.0.insert(source.clone(), load);
        }
        self.0[source].loaded.clone()
    }

    /// Loads `image` away from the UI, and redraws the view that asked for it.
    fn spawn(
        &self,
        source: &Source,
        window: &Window,
        cx: &mut Context<Self>,
        image: impl Future<Output = anyhow::Result<Image>> + Send + 'static,
    ) -> Load {
        let (source, view, renderer) = (source.clone(), window.current_view(), cx.svg_renderer());
        let task = cx.spawn(async move |this, cx| {
            let image = cx
                .background_spawn(async move { image.await.and_then(|image| image.to_image_data(renderer)) })
                .await;
            this.update(cx, |this, cx| {
                if let Some(load) = this.0.get_mut(&source) {
                    load.loaded = Some(image.map_err(Into::into));
                }
                App::notify(cx, view);
            })
            .ok();
        });
        Load { loaded: None, _task: task }
    }
}

/// A `data:` URL of an image format gpui decodes.
fn data_url_image(url: &str) -> Option<Image> {
    let data_url = DataUrl::process(url).ok()?;
    let mime = data_url.mime_type();
    let format = ImageFormat::from_mime_type(&format!("{}/{}", mime.type_, mime.subtype))?;
    let (bytes, _) = data_url.decode_to_vec().ok()?;
    Some(Image::from_bytes(format, bytes))
}

/// Match the preview's links: relative to the document, `/` relative to the task.
fn image_path(url: &str, root: &Path, document: &Path) -> Option<PathBuf> {
    if url.starts_with("//") || url.split(['/', '?', '#']).next()?.contains(':') {
        return None;
    }
    let path = url
        .split(['?', '#'])
        .next()
        .filter(|path| !path.is_empty())?;
    let path = percent_decode(path);
    Some(normalize(&match path.strip_prefix('/') {
        Some(path) => root.join(path),
        None => document.parent().unwrap_or(root).join(path),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use gpui_kit::component::text::{TextView, TextViewState};

    struct Preview {
        text: Entity<TextViewState>,
        logo: Arc<Image>,
        screenshot: Arc<Image>,
    }

    impl Render for Preview {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let logo = self.logo.clone();
            let screenshot = self.screenshot.clone();
            TextView::new(&self.text)
                .resolve_image_source(move |url| match url.as_ref() {
                    "packaging/macos/den.svg" => Some(logo.clone().into()),
                    "docs/screenshots/main.png" => Some(screenshot.clone().into()),
                    _ => None,
                })
                .scrollable(true)
                .size_full()
        }
    }

    #[gpui_kit::test]
    fn readme_html_images_reach_the_renderer(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let logo = Arc::new(Image::from_bytes(
            ImageFormat::Svg,
            include_bytes!("../../../../packaging/macos/den.svg").to_vec(),
        ));
        let screenshot = Arc::new(Image::from_bytes(
            ImageFormat::Png,
            include_bytes!("../../../../docs/screenshots/main.png").to_vec(),
        ));
        // The real README catches HTML <img> parsing as well as the resolver hook.
        let (_, cx) = cx.add_window_view(|_, cx| Preview {
            text: cx.new(|cx| TextViewState::markdown(include_str!("../../../../README.md"), cx)),
            logo: logo.clone(),
            screenshot: screenshot.clone(),
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert!(
                logo.clone().get_render_image(window, cx).is_some(),
                "the logo must decode and render"
            );
            assert!(
                screenshot.clone().get_render_image(window, cx).is_some(),
                "the screenshot must decode and render"
            );
        });
    }

    #[gpui_kit::test]
    fn unhandled_data_images_keep_the_default_decoder(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        // A 1x1 red PNG, also used by gpui-base's embedded-image test.
        let text = "![dot](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==)";
        let bytes = vec![
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207,
            192, 240, 31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
            96, 130,
        ];
        let image = Arc::new(Image::from_bytes(ImageFormat::Png, bytes));
        let (_, cx) = cx.add_window_view(|_, cx| Preview {
            text: cx.new(|cx| TextViewState::markdown(text, cx)),
            logo: image.clone(),
            screenshot: image.clone(),
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert!(
                image.is_asset_cached(cx),
                "unhandled data URLs must still decode inline"
            );
        });
    }

    /// A preview's image, a `data:` one here, stays on the GPU while a tab
    /// has its document, the source beside the preview too, and goes with
    /// the last one.
    #[gpui_kit::test]
    fn closing_a_document_frees_its_images(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(crate::config::Config::default());
        });
        let (workspace, cx) = cx.add_window_view(|window, cx| {
            window.activate_window();
            super::super::Workspace::new(PathBuf::from("/preview-test"), None, true, "preview-test".into(), window, cx)
        });
        let text = "![dot](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==)";
        cx.update(|window, cx| workspace.update(cx, |this, cx| {
            let tab = this.new_tab(PathBuf::from("/preview-test/README.md"), false, window, cx);
            tab.markdown.as_ref().unwrap().update(cx, |view, cx| view.set_text(text, cx));
            this.tabs.push(tab);
            this.tabs[0].content = super::super::Content::Ready;
            this.activate(0, window, cx);
            this.open_preview_to_side(&super::super::OpenPreviewToSide, window, cx);
        }));
        let painted = |cx: &mut VisualTestContext| {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
                let images = workspace.read(cx).tabs.last().map(|tab| tab.previewed.clone());
                images.and_then(|images| images.read(cx).0.values().find_map(|load| load.loaded.clone()))
            })
        };
        painted(cx);
        let image = painted(cx).expect("the preview loads its image").unwrap();
        assert!(cx.update(|window, _| window.has_image_atlas_entry(&image)));
        cx.update(|window, cx| workspace.update(cx, |this, cx| this.close(1, window, cx)));
        painted(cx);
        assert!(cx.update(|window, _| window.has_image_atlas_entry(&image)));
        cx.update(|window, cx| workspace.update(cx, |this, cx| this.close(0, window, cx)));
        painted(cx);
        assert!(!cx.update(|window, _| window.has_image_atlas_entry(&image)));
    }

    #[test]
    fn image_paths_follow_the_document_and_task() {
        let root = Path::new("/server/project");
        let document = root.join("docs/README.md");
        for (url, expected) in [
            ("../packaging/macos/den.svg", "packaging/macos/den.svg"),
            ("screenshots/main.png", "docs/screenshots/main.png"),
            ("/screenshots/main.png", "screenshots/main.png"),
            ("./a%20b%23c.png?raw=1#image", "docs/a b#c.png"),
        ] {
            assert_eq!(image_path(url, root, &document), Some(root.join(expected)));
        }
        for url in [
            "https://example.com/a.png",
            "data:image/png;base64,AA==",
            "file:///a.png",
            "//example.com/a.png",
            "#image",
            "",
        ] {
            assert_eq!(image_path(url, root, &document), None, "{url}");
        }
    }
}
