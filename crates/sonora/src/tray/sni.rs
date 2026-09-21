use ksni::blocking::{Handle, TrayMethods as _};
use ksni::menu::{CheckmarkItem, MenuItem, StandardItem};
use ksni::{Category, ToolTip};
use tokio::sync::mpsc::UnboundedSender;

use super::{Art, Event, Shown};

const ID: &str = "sonora";
const ICON_NAME: &str = "sonora";
const PNG: &[u8] = include_bytes!("../../../../assets/tray/sonora.png");
/// Flatpak writes this file into every sandbox it starts.
const FLATPAK_INFO: &str = "/.flatpak-info";

pub struct Icon {
    /// What a fresh service is spawned from when the icon comes back. It only follows `show`
    /// while the icon is out, since `Tray::place` publishes again right after the spawn.
    item: Item,
    handle: Option<Handle<Item>>,
}

impl Icon {
    pub fn new(sender: UnboundedSender<Event>) -> Option<Self> {
        let pixmap = match image::load_from_memory(PNG) {
            Ok(image) => {
                let image = image.into_rgba8();
                let (width, height) = image.dimensions();
                let mut data = image.into_raw();
                for pixel in data.as_chunks_mut::<4>().0 {
                    pixel.rotate_right(1);
                }
                vec![ksni::Icon {
                    width: width as i32,
                    height: height as i32,
                    data,
                }]
            }
            Err(error) => {
                log::warn!("tray: cannot decode the tray icon: {error:#}");
                Vec::new()
            }
        };
        let item = Item {
            sender,
            pixmap,
            shown: None,
        };
        let handle = spawn(item.clone())?;
        Some(Self {
            item,
            handle: Some(handle),
        })
    }

    /// A host draws every item that is registered, so leaving the bus is the only way out.
    pub fn placed(&mut self, placed: bool) {
        if placed == self.handle.is_some() {
            return;
        }
        match self.handle.take() {
            // the request is sent, not awaited: the item leaves the bus either way
            Some(handle) => drop(handle.shutdown()),
            None => self.handle = spawn(self.item.clone()),
        }
    }

    pub fn show(&mut self, shown: &Shown) {
        let shown = shown.clone();
        match &self.handle {
            Some(handle) => {
                handle.update(|item| item.shown = Some(shown));
            }
            None => self.item.shown = Some(shown),
        }
    }
}

fn spawn(item: Item) -> Option<Handle<Item>> {
    // A sandbox cannot own `org.kde.StatusNotifierItem-<pid>-<n>`, and a manifest cannot
    // grant it: flatpak's own-name wildcard only matches a `.*` suffix. The watcher
    // accepts the unique bus name instead.
    let sandboxed = std::path::Path::new(FLATPAK_INFO).exists();
    match item.disable_dbus_name(sandboxed).spawn() {
        Ok(handle) => Some(handle),
        Err(error) => {
            log::warn!("tray: cannot reach the status notifier host: {error}");
            None
        }
    }
}

#[derive(Clone)]
struct Item {
    sender: UnboundedSender<Event>,
    pixmap: Vec<ksni::Icon>,
    shown: Option<Shown>,
}

impl Item {
    fn send(&self, event: Event) {
        self.sender.send(event).ok();
    }

    fn entry(&self, label: &str, event: Event) -> MenuItem<Self> {
        StandardItem {
            label: label.to_owned(),
            activate: Box::new(move |this: &mut Self| this.send(event)),
            ..Default::default()
        }
        .into()
    }

    fn checkmark(&self, label: &str, checked: bool, event: Event) -> MenuItem<Self> {
        CheckmarkItem {
            label: label.to_owned(),
            checked,
            activate: Box::new(move |this: &mut Self| this.send(event)),
            ..Default::default()
        }
        .into()
    }
}

impl ksni::Tray for Item {
    fn id(&self) -> String {
        ID.to_owned()
    }

    fn category(&self) -> Category {
        Category::ApplicationStatus
    }

    fn title(&self) -> String {
        "Sonora".to_owned()
    }

    fn icon_name(&self) -> String {
        ICON_NAME.to_owned()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.pixmap.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Sonora".to_owned(),
            description: self
                .shown
                .as_ref()
                .map(|shown| shown.caption.clone())
                .unwrap_or_default(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Event::Show);
    }

    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        self.send(Event::Toggle);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let Some(shown) = &self.shown else {
            return Vec::new();
        };
        vec![
            StandardItem {
                label: shown.caption.clone(),
                icon_data: cover(shown.artwork.as_ref()).unwrap_or_default(),
                enabled: shown.song,
                activate: Box::new(|this: &mut Self| this.send(Event::Song)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            self.entry(&shown.toggle, Event::Toggle),
            self.entry(&shown.previous, Event::Previous),
            self.entry(&shown.next, Event::Next),
            MenuItem::Separator,
            self.checkmark(&shown.shuffle, shown.shuffle_on, Event::Shuffle),
            self.checkmark(&shown.repeat, shown.repeat_on, Event::Repeat),
            MenuItem::Separator,
            self.entry(&shown.show, Event::Show),
            self.entry(&shown.quit, Event::Quit),
        ]
    }
}

/// The cover as the png a menu item carries. A cover that cannot be encoded is simply left off
/// the row.
fn cover(art: Option<&Art>) -> Option<Vec<u8>> {
    let art = art?;
    let image = image::RgbaImage::from_raw(art.width, art.height, art.data.clone())?;

    let mut png = Vec::new();
    let written = image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
    match written {
        Ok(()) => Some(png),
        Err(error) => {
            log::warn!("tray: cannot encode the cover: {error:#}");
            None
        }
    }
}
