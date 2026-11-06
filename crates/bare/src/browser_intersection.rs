use crate::browser::*;
use bare_js::eval::Interp;
impl Dom {
    pub(crate) fn io_wants_frame(&self) -> bool { false }
    pub(crate) fn io_frame(&self, _: &mut Interp<'_>) -> R<()> { Ok(()) }
}
