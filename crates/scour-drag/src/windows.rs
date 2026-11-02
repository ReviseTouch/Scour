//! A drag on Windows, by OLE: the files as `CF_HDROP`, which Explorer, a
//! mail client and a browser's upload field all read. `DoDragDrop` runs its
//! own loop until the drop is over, so a drag here returns when it ends.

use std::path::Path;

use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC, E_NOTIMPL,
    HGLOBAL, OLE_E_ADVISENOTSUPPORTED, POINT, S_OK,
};
use windows::Win32::System::Com::{
    DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject, IDataObject_Impl,
    IEnumFORMATETC, IEnumSTATDATA, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalUnlock,
};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::{DROPFILES, SHCreateStdEnumFmtEtc};
use windows::core::{BOOL, HRESULT, Ref, implement};

use crate::Refused;

#[derive(Debug, Default)]
pub(crate) struct Windows {
    dropped: bool,
}

impl Windows {
    pub fn files(&mut self, paths: &[&Path]) -> Result<(), Refused> {
        // Already done by the toolkit on this thread, most likely; once more
        // is harmless and answers S_FALSE.
        // SAFETY: no arguments that outlive the call.
        let _ = unsafe { OleInitialize(None) };
        let data: IDataObject = Files::new(paths).into();
        let source: IDropSource = Source.into();
        let mut effect = DROPEFFECT(0);
        // SAFETY: both objects live across the call; `effect` is written once.
        let r = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
        if r.is_err() {
            return Err(Refused::Broken);
        }
        self.dropped = r == DRAGDROP_S_DROP && effect != DROPEFFECT(0);
        Ok(())
    }

    pub fn dropped(&self) -> bool {
        self.dropped
    }
}

/// The files, held as the `DROPFILES` block a drop target is handed a copy of.
#[implement(IDataObject)]
struct Files {
    block: Vec<u8>,
}

impl Files {
    fn new(paths: &[&Path]) -> Files {
        let header = std::mem::size_of::<DROPFILES>();
        let mut block = vec![0u8; header];
        let head = DROPFILES {
            pFiles: header as u32,
            pt: POINT { x: 0, y: 0 },
            fNC: BOOL(0),
            fWide: BOOL(1),
        };
        // SAFETY: `block` is exactly a `DROPFILES` long; written unaligned.
        unsafe { std::ptr::write_unaligned(block.as_mut_ptr().cast::<DROPFILES>(), head) };
        // Each path in UTF-16 and ended by a NUL, the list by one more.
        for p in paths {
            for unit in p.as_os_str().to_string_lossy().encode_utf16().chain([0]) {
                block.extend_from_slice(&unit.to_le_bytes());
            }
        }
        block.extend_from_slice(&[0, 0]);
        Files { block }
    }

    fn wanted(format: &FORMATETC) -> bool {
        format.cfFormat == CF_HDROP.0
            && format.dwAspect == DVASPECT_CONTENT.0
            && format.tymed & TYMED_HGLOBAL.0 as u32 != 0
    }

    fn format() -> FORMATETC {
        FORMATETC {
            cfFormat: CF_HDROP.0,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        }
    }
}

impl IDataObject_Impl for Files_Impl {
    fn GetData(&self, format: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        // SAFETY: OLE hands a valid FORMATETC.
        let format = unsafe { format.as_ref() }.ok_or(DV_E_FORMATETC)?;
        if !Files::wanted(format) {
            return Err(DV_E_FORMATETC.into());
        }
        // SAFETY: a fresh block, filled while locked; the receiver frees it.
        let global: HGLOBAL =
            unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, self.block.len())? };
        unsafe {
            let at = GlobalLock(global);
            if at.is_null() {
                return Err(E_NOTIMPL.into());
            }
            std::ptr::copy_nonoverlapping(self.block.as_ptr(), at.cast::<u8>(), self.block.len());
            let _ = GlobalUnlock(global);
        }
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: global },
            pUnkForRelease: std::mem::ManuallyDrop::new(None),
        })
    }

    fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, format: *const FORMATETC) -> HRESULT {
        // SAFETY: OLE hands a valid FORMATETC.
        match unsafe { format.as_ref() } {
            Some(f) if Files::wanted(f) => S_OK,
            _ => DV_E_FORMATETC,
        }
    }

    fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, out: *mut FORMATETC) -> HRESULT {
        // SAFETY: OLE hands somewhere to write; nothing canonical differs.
        if let Some(out) = unsafe { out.as_mut() } {
            out.ptd = std::ptr::null_mut();
        }
        E_NOTIMPL
    }

    fn SetData(
        &self,
        _: *const FORMATETC,
        _: *const STGMEDIUM,
        _: BOOL,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, direction: u32) -> windows::core::Result<IEnumFORMATETC> {
        if direction != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        // SAFETY: one format, copied by the shell.
        unsafe { SHCreateStdEnumFmtEtc(&[Files::format()]) }
    }

    fn DAdvise(
        &self,
        _: *const FORMATETC,
        _: u32,
        _: Ref<'_, IAdviseSink>,
    ) -> windows::core::Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _: u32) -> windows::core::Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

/// When the drag ends: the button let go drops, Escape cancels.
#[implement(IDropSource)]
struct Source;

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}
