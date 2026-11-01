//! Bounded waiting on WinRT asynchronous operations, and HRESULT classification.
use crate::model::{PlatformError, PlatformErrorKind};
use crate::platform::Deadline;
use std::sync::mpsc::channel;
use windows::core::RuntimeType;
use windows_future::{AsyncOperationCompletedHandler, AsyncStatus, IAsyncOperation};

/// Classify a Windows error without losing the HRESULT.
pub(super) fn classify(err: &windows::core::Error, context: &'static str) -> PlatformError {
    // Values are the documented HRESULTs, compared as the unsigned bit pattern.
    let code = err.code().0;
    let kind = match code as u32 {
        0x8007_0005 => PlatformErrorKind::AccessDenied, // E_ACCESSDENIED
        0x8007_0490 | 0x8007_0002 | 0x8007_0003 | 0x8000_000B => PlatformErrorKind::NotFound,
        // ERROR_NOT_FOUND, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, E_BOUNDS
        0x8007_0057 => PlatformErrorKind::InvalidArgument, // E_INVALIDARG
        0x8000_4004 | 0x8007_04C7 => PlatformErrorKind::Canceled, // E_ABORT, ERROR_CANCELLED
        0x8004_0154 | 0x8000_4001 | 0x8007_0032 | 0x8007_007F | 0x8000_4002 => {
            PlatformErrorKind::Unsupported
        } // REGDB_E_CLASSNOTREG, E_NOTIMPL, ERROR_NOT_SUPPORTED, ERROR_PROC_NOT_FOUND, E_NOINTERFACE
        0x8000_4003 => PlatformErrorKind::NotAvailable, // E_POINTER (null object)
        _ => PlatformErrorKind::Os,
    };
    PlatformError::new(kind, context).with_hresult(code)
}

/// Wait for `op` until `deadline`. On expiry the operation is asked to cancel, but whether it had
/// an effect is unknown, so the caller must treat a timeout on a mutating operation as uncertain.
pub(super) fn await_op<T>(
    op: windows::core::Result<IAsyncOperation<T>>,
    deadline: Deadline,
    context: &'static str,
) -> Result<T, PlatformError>
where
    T: RuntimeType + 'static,
{
    let op = op.map_err(|e| classify(&e, context))?;
    let (tx, rx) = channel::<AsyncStatus>();
    // SetCompleted invokes the handler at once if the operation already finished.
    let handler = AsyncOperationCompletedHandler::<T>::new(move |_op, status| {
        let _ = tx.send(status);
        Ok(())
    });
    op.SetCompleted(&handler)
        .map_err(|e| classify(&e, context))?;
    match rx.recv_timeout(deadline.remaining()) {
        Ok(AsyncStatus::Canceled) => Err(PlatformError::new(PlatformErrorKind::Canceled, context)),
        // Completed and Error both resolve through GetResults, which carries the failure code.
        Ok(_) => op.GetResults().map_err(|e| classify(&e, context)),
        Err(_) => {
            let _ = op.Cancel();
            Err(PlatformError::new(PlatformErrorKind::TimedOut, context))
        }
    }
}
