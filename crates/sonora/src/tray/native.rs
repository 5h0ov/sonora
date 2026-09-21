use tokio::sync::mpsc::UnboundedSender;
use tray_icon::menu::{
    CheckMenuItem, Icon as MenuIcon, IconMenuItem, Menu, MenuEvent, MenuId, MenuItem,
    PredefinedMenuItem,
};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use super::{Art, Event, Shown};

const TOOLTIP: &str = "Sonora";
const PNG: &[u8] = match cfg!(target_os = "macos") {
    true => include_bytes!("../../../../assets/tray/template-64.png"),
    false => include_bytes!("../../../../assets/tray/sonora.png"),
};
const MENU_ON_CLICK: bool = cfg!(target_os = "macos");

pub struct Icon {
    icon: TrayIcon,
    caption: IconMenuItem,
    toggle: MenuItem,
    previous: MenuItem,
    next: MenuItem,
    shuffle: CheckMenuItem,
    repeat: CheckMenuItem,
    show: MenuItem,
    quit: MenuItem,
}

impl Icon {
    pub fn new(sender: UnboundedSender<Event>) -> Option<Self> {
        let image = match image::load_from_memory(PNG) {
            Ok(image) => image.into_rgba8(),
            Err(error) => {
                log::warn!("tray: cannot decode the tray icon: {error:#}");
                return None;
            }
        };
        let (width, height) = image.dimensions();
        let icon = match tray_icon::Icon::from_rgba(image.into_raw(), width, height) {
            Ok(icon) => icon,
            Err(error) => {
                log::warn!("tray: cannot build the tray icon: {error:#}");
                return None;
            }
        };

        let caption = IconMenuItem::with_id("caption", "", false, None, None);
        let toggle = MenuItem::with_id("toggle", "", true, None);
        let previous = MenuItem::with_id("previous", "", true, None);
        let next = MenuItem::with_id("next", "", true, None);
        let shuffle = CheckMenuItem::with_id("shuffle", "", true, false, None);
        let repeat = CheckMenuItem::with_id("repeat", "", true, false, None);
        let show = MenuItem::with_id("show", "", true, None);
        let quit = MenuItem::with_id("quit", "", true, None);
        let menu = Menu::new();
        if let Err(error) = menu.append_items(&[
            &caption,
            &PredefinedMenuItem::separator(),
            &toggle,
            &previous,
            &next,
            &PredefinedMenuItem::separator(),
            &shuffle,
            &repeat,
            &PredefinedMenuItem::separator(),
            &show,
            &quit,
        ]) {
            log::warn!("tray: cannot build the tray menu: {error:#}");
            return None;
        }

        let built = TrayIconBuilder::new()
            .with_icon(icon)
            .with_icon_as_template(true)
            .with_tooltip(TOOLTIP)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(MENU_ON_CLICK)
            .build();
        let icon = match built {
            Ok(icon) => icon,
            Err(error) => {
                log::warn!("tray: cannot place the tray icon: {error:#}");
                return None;
            }
        };
        #[cfg(target_os = "macos")]
        keep_cover(&icon);

        let menus = sender.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some(event) = translate(&event.id) {
                menus.send(event).ok();
            }
        }));
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            else {
                return;
            };
            if !MENU_ON_CLICK {
                sender.send(Event::Show).ok();
            }
        }));

        Some(Self {
            icon,
            caption,
            toggle,
            previous,
            next,
            shuffle,
            repeat,
            show,
            quit,
        })
    }

    /// The status item stays alive either way, so the menu and its handlers survive a round trip.
    pub fn placed(&mut self, placed: bool) {
        if let Err(error) = self.icon.set_visible(placed) {
            log::warn!("tray: cannot place the tray icon: {error:#}");
        }
    }

    pub fn show(&mut self, shown: &Shown) {
        // the status notifier hosts read the caption off the tooltip themselves; here it has to
        // be pushed, or hovering the icon only ever says Sonora
        if let Err(error) = self.icon.set_tooltip(Some(&shown.caption)) {
            log::warn!("tray: cannot set the tooltip: {error:#}");
        }
        self.caption.set_text(&shown.caption);
        self.caption.set_icon(cover(shown.artwork.as_ref()));
        self.caption.set_enabled(shown.song);
        self.toggle.set_text(&shown.toggle);
        self.previous.set_text(&shown.previous);
        self.next.set_text(&shown.next);
        self.shuffle.set_text(&shown.shuffle);
        self.shuffle.set_checked(shown.shuffle_on);
        self.repeat.set_text(&shown.repeat);
        self.repeat.set_checked(shown.repeat_on);
        self.show.set_text(&shown.show);
        self.quit.set_text(&shown.quit);
    }
}

/// The cover as a menu icon. A cover that cannot be built is simply left off the row.
fn cover(art: Option<&Art>) -> Option<MenuIcon> {
    let art = art?;
    match MenuIcon::from_rgba(art.data.clone(), art.width, art.height) {
        Ok(icon) => Some(icon),
        Err(error) => {
            log::warn!("tray: cannot build the cover icon: {error:#}");
            None
        }
    }
}

/// Keeps the cover on the caption row. From macOS 27 AppKit decides on its own whether a menu
/// item shows its image and typically hides it, so the caption asks for its image to stay. Older
/// systems do not know the selector and always show it.
#[cfg(target_os = "macos")]
fn keep_cover(icon: &TrayIcon) {
    use objc2::runtime::NSObjectProtocol as _;
    use objc2::{MainThreadMarker, msg_send, sel};

    const VISIBLE: isize = 1;
    let Some(mtm) = MainThreadMarker::new() else {
        return log::warn!("tray: the tray menu was built off the main thread");
    };
    let Some(caption) = icon
        .ns_status_item()
        .and_then(|item| item.menu(mtm))
        .and_then(|menu| menu.itemAtIndex(0))
    else {
        return log::warn!("tray: the caption row is missing from the tray menu");
    };
    if !caption.respondsToSelector(sel!(setPreferredImageVisibility:)) {
        return;
    }
    unsafe { msg_send![&*caption, setPreferredImageVisibility: VISIBLE] }
}

fn translate(id: &MenuId) -> Option<Event> {
    Some(match id.0.as_str() {
        "caption" => Event::Song,
        "toggle" => Event::Toggle,
        "previous" => Event::Previous,
        "next" => Event::Next,
        "shuffle" => Event::Shuffle,
        "repeat" => Event::Repeat,
        "show" => Event::Show,
        "quit" => Event::Quit,
        _ => return None,
    })
}
