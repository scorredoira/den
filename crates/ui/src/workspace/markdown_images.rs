//! Markdown file images belong to the document's machine, including over SSH.
use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Arc,
};

use client::Client;
use gpui_kit::*;
use proto::{Request, Response};

use super::{image_format, normalize, percent_decode};

pub(super) fn resolver(
    client: Option<Arc<Client>>,
    root: PathBuf,
    document: PathBuf,
) -> impl Fn(&SharedUri) -> Option<ImageSource> + Send + Sync + 'static {
    move |url| {
        let path = image_path(url.as_ref(), &root, &document)?;
        let source = FileImage {
            client: client.clone(),
            path,
        };
        Some(ImageSource::Custom(Arc::new(move |window, cx| {
            window.use_asset::<AssetLogger<FileImageLoader>>(&source, cx)
        })))
    }
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

#[derive(Clone)]
struct FileImage {
    client: Option<Arc<Client>>,
    path: PathBuf,
}

impl Hash for FileImage {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // The same path on two different servers must not share cached pixels.
        self.client.as_ref().map(Arc::as_ptr).hash(state);
        self.path.hash(state);
    }
}

struct FileImageLoader;

impl Asset for FileImageLoader {
    type Source = FileImage;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        source: FileImage,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let renderer = cx.svg_renderer();
        async move {
            let client = source
                .client
                .ok_or_else(|| anyhow::anyhow!("No agent for Markdown image"))?;
            let format = image_format(&source.path)
                .ok_or_else(|| anyhow::anyhow!("Unsupported image: {}", source.path.display()))?;
            match client
                .request(Request::ReadFile { path: source.path })
                .await?
            {
                Response::Bytes(bytes) => Image::from_bytes(format, bytes)
                    .to_image_data(renderer)
                    .map_err(Into::into),
                other => Err(anyhow::anyhow!("Unexpected image response: {other:?}").into()),
            }
        }
    }
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
