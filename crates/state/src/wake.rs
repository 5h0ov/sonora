use gpui::{Context, Entity};
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use tokio::sync::watch;

use crate::{Io, Playback, PlaybackState};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Want {
    system: bool,
    display: bool,
}

pub struct Wake {
    playback: Entity<Playback>,
    fullscreen: bool,
    focused: bool,
    applied: Want,
    #[cfg(target_os = "macos")]
    assertions: Assertions,
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    sender: watch::Sender<Want>,
}

impl Wake {
    pub fn new(playback: Entity<Playback>, io: Io, cx: &mut Context<Self>) -> Self {
        cx.observe(&playback, |this, _, cx| this.apply(cx)).detach();

        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        let sender = {
            let (sender, receiver) = watch::channel(Want::default());
            io.spawn(inhibit(receiver));
            sender
        };
        #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
        let _ = io;

        Self {
            playback,
            fullscreen: false,
            focused: false,
            applied: Want::default(),
            #[cfg(target_os = "macos")]
            assertions: Assertions::default(),
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            sender,
        }
    }

    pub fn set_fullscreen(&mut self, on: bool, cx: &mut Context<Self>) {
        self.fullscreen = on;
        self.apply(cx);
    }

    pub fn set_focused(&mut self, on: bool, cx: &mut Context<Self>) {
        self.focused = on;
        self.apply(cx);
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        let playing = *self.playback.read(cx).state() == PlaybackState::Playing;
        let want = Want {
            system: playing,
            display: playing && self.fullscreen && self.focused,
        };
        if want == self.applied {
            return;
        }
        self.applied = want;
        self.hold(want);
    }

    fn hold(&mut self, want: Want) {
        #[cfg(target_os = "windows")]
        {
            use windows_sys::Win32::System::Power::{
                ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
            };

            let mut flags = ES_CONTINUOUS;
            if want.system {
                flags |= ES_SYSTEM_REQUIRED;
            }
            if want.display {
                flags |= ES_DISPLAY_REQUIRED;
            }
            // SAFETY: called only on the main thread and only the documented request flags are passed.
            if unsafe { SetThreadExecutionState(flags) } == 0 {
                log::warn!("wake: cannot set the execution state");
            }
        }

        #[cfg(target_os = "macos")]
        self.assertions.set(want);

        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        self.sender.send_if_modified(|current| {
            if *current == want {
                return false;
            }
            *current = want;
            true
        });

        #[cfg(not(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux",
            target_os = "freebsd"
        )))]
        let _ = want;
    }
}

/// The two assertions on macOS, held by id once created
#[cfg(target_os = "macos")]
#[derive(Default)]
struct Assertions {
    system: Option<objc2_io_kit::IOPMAssertionID>,
    display: Option<objc2_io_kit::IOPMAssertionID>,
}

#[cfg(target_os = "macos")]
impl Assertions {
    fn set(&mut self, want: Want) {
        hold_one(&mut self.system, want.system, "PreventUserIdleSystemSleep");
        hold_one(
            &mut self.display,
            want.display,
            "PreventUserIdleDisplaySleep",
        );
    }
}

#[cfg(target_os = "macos")]
fn hold_one(held: &mut Option<objc2_io_kit::IOPMAssertionID>, on: bool, kind: &str) {
    use objc2_core_foundation::CFString;
    use objc2_io_kit::{
        IOPMAssertionCreateWithName, IOPMAssertionRelease, kIOPMAssertionLevelOn, kIOReturnSuccess,
    };

    if on == held.is_some() {
        return;
    }
    if on {
        let kind = CFString::from_str(kind);
        let name = CFString::from_str("Music is playing");
        let mut id = 0;
        // SAFETY: both strings outlive the call and `id` is a valid out pointer.
        let result = unsafe {
            IOPMAssertionCreateWithName(Some(&kind), kIOPMAssertionLevelOn, Some(&name), &mut id)
        };
        match result == kIOReturnSuccess {
            true => *held = Some(id),
            false => log::warn!("wake: cannot hold a power assertion: {result}"),
        }
    } else if let Some(id) = held.take() {
        let _ = IOPMAssertionRelease(id);
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
async fn inhibit(mut receiver: watch::Receiver<Want>) {
    use ashpd::desktop::Request;

    let mut held: Option<Request<()>> = None;
    let mut applied = Want::default();
    while receiver.changed().await.is_ok() {
        let want = *receiver.borrow_and_update();
        if want == applied {
            continue;
        }
        if let Some(request) = held.take()
            && let Err(error) = request.close().await
        {
            log::warn!("wake: cannot release the inhibitor: {error}");
        }
        if want.system || want.display {
            held = acquire(want).await;
        }
        applied = want;
    }
    if let Some(request) = held {
        let _ = request.close().await;
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
async fn acquire(want: Want) -> Option<ashpd::desktop::Request<()>> {
    use ashpd::desktop::inhibit::{InhibitFlags, InhibitOptions, InhibitProxy};
    use ashpd::enumflags2::BitFlag;

    let proxy = match InhibitProxy::new().await {
        Ok(proxy) => proxy,
        Err(error) => {
            log::warn!("wake: cannot reach the inhibit portal: {error}");
            return None;
        }
    };
    let mut flags = InhibitFlags::empty();
    if want.system {
        flags.insert(InhibitFlags::Suspend);
    }
    if want.display {
        flags.insert(InhibitFlags::Idle);
    }
    let options = InhibitOptions::default().set_reason("Music is playing");
    match proxy.inhibit(None, flags, options).await {
        Ok(request) => Some(request),
        Err(error) => {
            log::warn!("wake: cannot inhibit: {error}");
            None
        }
    }
}
