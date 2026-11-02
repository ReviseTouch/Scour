//! A drag on macOS, by AppKit: a dragging session begun on the window's view
//! with the mouse-down being handled, each file an `NSURL`. The session runs
//! in the application's own event loop and tells the source when it ends.

use std::cell::Cell;
use std::ffi::c_void;
use std::path::Path;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send,
};
use objc2_app_kit::{
    NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
    NSDraggingSource, NSView,
};
use objc2_foundation::{
    NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
};

use crate::Refused;

pub(crate) struct Mac {
    view: Retained<NSView>,
    busy: Rc<Cell<bool>>,
    dropped: Rc<Cell<bool>>,
    /// Kept while a session lives: AppKit holds its source weakly.
    source: Option<Retained<Source>>,
}

impl std::fmt::Debug for Mac {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mac")
            .field("dragging", &self.busy.get())
            .finish_non_exhaustive()
    }
}

struct Ivars {
    busy: Rc<Cell<bool>>,
    dropped: Rc<Cell<bool>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Source` adds none.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ScourDragSource"]
    #[ivars = Ivars]
    struct Source;

    unsafe impl NSObjectProtocol for Source {}

    unsafe impl NSDraggingSource for Source {
        /// A copy, never a move: a file found is not a file to take away.
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn ended(&self, _session: &NSDraggingSession, _at: NSPoint, op: NSDragOperation) {
            self.ivars().dropped.set(op != NSDragOperation::None);
            self.ivars().busy.set(false);
        }
    }
);

impl Source {
    fn new(mtm: MainThreadMarker, busy: Rc<Cell<bool>>, dropped: Rc<Cell<bool>>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { busy, dropped });
        // SAFETY: NSObject's designated initialiser.
        unsafe { msg_send![super(this), init] }
    }
}

impl Mac {
    /// # Safety
    ///
    /// `view` must be the window's live `NSView`.
    pub unsafe fn new(view: *mut c_void) -> Option<Mac> {
        // SAFETY: the caller vouches for the view; it is retained here.
        let view: &NSView = unsafe { view.cast::<NSView>().as_ref()? };
        Some(Mac {
            view: view.retain(),
            busy: Rc::default(),
            dropped: Rc::default(),
            source: None,
        })
    }

    pub fn files(&mut self, paths: &[&Path]) -> Result<(), Refused> {
        let mtm = MainThreadMarker::new().ok_or(Refused::Unsupported)?;
        // The press being handled: a session begins from a mouse event.
        let event = NSApplication::sharedApplication(mtm)
            .currentEvent()
            .ok_or(Refused::NoPress)?;
        let at = self
            .view
            .convertPoint_fromView(event.locationInWindow(), None);
        let frame = NSRect::new(
            NSPoint::new(at.x - 16.0, at.y - 16.0),
            NSSize::new(32.0, 32.0),
        );
        let items: Vec<Retained<NSDraggingItem>> = paths
            .iter()
            .map(|p| {
                let url = NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy()));
                let item = NSDraggingItem::initWithPasteboardWriter(
                    NSDraggingItem::alloc(),
                    ProtocolObject::from_ref(&*url),
                );
                // SAFETY: no contents image; the frame is in the view's coordinates.
                unsafe { item.setDraggingFrame_contents(frame, None) };
                item
            })
            .collect();
        let source = Source::new(mtm, Rc::clone(&self.busy), Rc::clone(&self.dropped));
        self.busy.set(true);
        self.dropped.set(false);
        let _session = self.view.beginDraggingSessionWithItems_event_source(
            &NSArray::from_retained_slice(&items),
            &event,
            ProtocolObject::from_ref(&*source),
        );
        self.source = Some(source);
        Ok(())
    }

    pub fn dragging(&self) -> bool {
        self.busy.get()
    }

    pub fn dropped(&self) -> bool {
        self.dropped.get()
    }
}
