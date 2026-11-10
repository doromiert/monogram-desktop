//! Small FFI boundary to the exact upstream ratchet used by mobile.
use super::{records::*, *};
use std::{
    cell::RefCell,
    ffi::{c_char, c_void},
    ptr,
};
#[repr(C)]
struct Buffer {
    _private: [u8; 0],
}
#[repr(C)]
struct Callbacks {
    data: *mut c_void,
    load: unsafe extern "C" fn(*mut c_void, i32, u32, *mut *mut Buffer) -> i32,
    save: unsafe extern "C" fn(*mut c_void, i32, u32, *const u8, usize) -> i32,
}
unsafe extern "C" {
    fn mg_buffer_new(p: *const u8, n: usize) -> *mut Buffer;
    fn mg_buffer_data(p: *mut Buffer) -> *const u8;
    fn mg_buffer_len(p: *mut Buffer) -> usize;
    fn mg_buffer_free(p: *mut Buffer);
    fn mg_identity(record: *mut *mut Buffer, public: *mut *mut Buffer) -> i32;
    fn mg_pre_key(id: u32, record: *mut *mut Buffer, public: *mut *mut Buffer) -> i32;
    fn mg_signed_key(
        identity: *const u8,
        length: usize,
        id: u32,
        time: u64,
        record: *mut *mut Buffer,
        public: *mut *mut Buffer,
        signature: *mut *mut Buffer,
    ) -> i32;
    fn mg_build(
        store: *const Callbacks,
        local: u32,
        peer: *const c_char,
        peer_len: usize,
        device: u32,
        pre_id: u32,
        pre: *const u8,
        signed_id: u32,
        signed: *const u8,
        signature: *const u8,
        identity: *const u8,
    ) -> i32;
    fn mg_encrypt(
        store: *const Callbacks,
        local: u32,
        peer: *const c_char,
        peer_len: usize,
        device: u32,
        plain: *const u8,
        length: usize,
        pre_key: *mut u8,
        out: *mut *mut Buffer,
    ) -> i32;
    fn mg_decrypt(
        store: *const Callbacks,
        local: u32,
        peer: *const c_char,
        peer_len: usize,
        device: u32,
        bytes: *const u8,
        length: usize,
        pre_key: u8,
        out: *mut *mut Buffer,
    ) -> i32;
}
struct Owned(*mut Buffer);
impl Owned {
    fn empty() -> Self {
        Self(ptr::null_mut())
    }
    fn bytes(&self) -> Result<Secret> {
        if self.0.is_null() {
            return Err(Error::Native(-12));
        }
        // The adapter returns a live buffer owned by this guard, bounded at 1 MiB.
        unsafe {
            let n = mg_buffer_len(self.0);
            if n > 1024 * 1024 {
                return Err(Error::Invalid("native buffer"));
            }
            if n == 0 {
                return Ok(Secret::new(vec![]));
            }
            Ok(Secret::new(
                std::slice::from_raw_parts(mg_buffer_data(self.0), n).to_vec(),
            ))
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { mg_buffer_free(self.0) }
    }
}
fn checked(r: i32) -> Result<()> {
    if r < 0 { Err(Error::Native(r)) } else { Ok(()) }
}
pub fn identity() -> Result<(Secret, [u8; 32])> {
    let (mut r, mut p) = (Owned::empty(), Owned::empty());
    unsafe {
        checked(mg_identity(&mut r.0, &mut p.0))?;
    }
    Ok((
        r.bytes()?,
        p.bytes()?
            .as_slice()
            .try_into()
            .map_err(|_| Error::Invalid("identity"))?,
    ))
}
pub fn pre_key(id: u32) -> Result<(Secret, [u8; 32])> {
    let (mut r, mut p) = (Owned::empty(), Owned::empty());
    unsafe {
        checked(mg_pre_key(id, &mut r.0, &mut p.0))?;
    }
    Ok((
        r.bytes()?,
        p.bytes()?
            .as_slice()
            .try_into()
            .map_err(|_| Error::Invalid("prekey"))?,
    ))
}
pub fn signed_key(identity: &[u8], id: u32, time: u64) -> Result<(Secret, [u8; 32], [u8; 64])> {
    let (mut r, mut p, mut s) = (Owned::empty(), Owned::empty(), Owned::empty());
    unsafe {
        checked(mg_signed_key(
            identity.as_ptr(),
            identity.len(),
            id,
            time,
            &mut r.0,
            &mut p.0,
            &mut s.0,
        ))?;
    }
    Ok((
        r.bytes()?,
        p.bytes()?
            .as_slice()
            .try_into()
            .map_err(|_| Error::Invalid("signed key"))?,
        s.bytes()?
            .as_slice()
            .try_into()
            .map_err(|_| Error::Invalid("signature"))?,
    ))
}
struct Context<'a, 'tx> {
    tx: &'a Tx<'tx>,
    peer: &'a str,
    error: RefCell<Option<Error>>,
}
impl Context<'_, '_> {
    fn fail(&self, e: Error) -> i32 {
        self.error.replace(Some(e));
        -1
    }
    fn result(&self, r: i32) -> Result<()> {
        if let Some(e) = self.error.borrow_mut().take() {
            Err(e)
        } else {
            checked(r)
        }
    }
    fn callbacks(&self) -> Callbacks {
        Callbacks {
            data: ptr::from_ref(self) as *mut c_void,
            load,
            save,
        }
    }
}
unsafe extern "C" fn load(p: *mut c_void, kind: i32, id: u32, out: *mut *mut Buffer) -> i32 {
    // The stack context stays live for the synchronous native operation; no callback escapes.
    let ctx = unsafe { &*(p as *const Context<'_, '_>) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i32> {
        unsafe { *out = ptr::null_mut() };
        let Some(bytes) = ctx
            .tx
            .load(kind, if kind >= SESSION { ctx.peer } else { "" }, id)?
        else {
            return Ok(0);
        };
        let buffer = unsafe { mg_buffer_new(bytes.as_ptr(), bytes.len()) };
        if buffer.is_null() {
            return Err(Error::Native(-12));
        }
        unsafe { *out = buffer };
        Ok(1)
    }));
    match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => ctx.fail(e),
        Err(_) => ctx.fail(Error::Invalid("store callback")),
    }
}
unsafe extern "C" fn save(p: *mut c_void, kind: i32, id: u32, data: *const u8, n: usize) -> i32 {
    let ctx = unsafe { &*(p as *const Context<'_, '_>) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        if n > 1024 * 1024 {
            return Err(Error::Invalid("native record"));
        }
        let value = if data.is_null() {
            None
        } else {
            Some(unsafe { std::slice::from_raw_parts(data, n) })
        };
        ctx.tx
            .save(kind, if kind >= SESSION { ctx.peer } else { "" }, id, value)
    }));
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => ctx.fail(e),
        Err(_) => ctx.fail(Error::Invalid("store callback")),
    }
}
pub fn build(
    tx: &Tx<'_>,
    local: u32,
    peer: &str,
    device: u32,
    pre_id: u32,
    pre: &[u8; 32],
    bundle: &super::bundle::Bundle,
) -> Result<()> {
    address(peer, device)?;
    let ctx = Context {
        tx,
        peer,
        error: RefCell::new(None),
    };
    let r = unsafe {
        mg_build(
            &ctx.callbacks(),
            local,
            peer.as_ptr().cast(),
            peer.len(),
            device,
            pre_id,
            pre.as_ptr(),
            bundle.signed_id,
            bundle.signed.as_ptr(),
            bundle.signature.as_ptr(),
            bundle.identity.as_ptr(),
        )
    };
    ctx.result(r)
}
pub fn encrypt(
    tx: &Tx<'_>,
    local: u32,
    peer: &str,
    device: u32,
    plaintext: &[u8],
) -> Result<(bool, Vec<u8>)> {
    address(peer, device)?;
    if plaintext.is_empty() || plaintext.len() > 1024 * 1024 {
        return Err(Error::Invalid("plaintext size"));
    }
    let ctx = Context {
        tx,
        peer,
        error: RefCell::new(None),
    };
    let mut out = Owned::empty();
    let mut flag = 0;
    let r = unsafe {
        mg_encrypt(
            &ctx.callbacks(),
            local,
            peer.as_ptr().cast(),
            peer.len(),
            device,
            plaintext.as_ptr(),
            plaintext.len(),
            &mut flag,
            &mut out.0,
        )
    };
    ctx.result(r)?;
    Ok((flag == 1, out.bytes()?.to_vec()))
}
pub fn decrypt(
    tx: &Tx<'_>,
    local: u32,
    peer: &str,
    device: u32,
    ciphertext: &[u8],
    pre_key: bool,
) -> Result<Secret> {
    address(peer, device)?;
    if ciphertext.is_empty() || ciphertext.len() > 1024 * 1024 {
        return Err(Error::Invalid("ciphertext size"));
    }
    let ctx = Context {
        tx,
        peer,
        error: RefCell::new(None),
    };
    let mut out = Owned::empty();
    let r = unsafe {
        mg_decrypt(
            &ctx.callbacks(),
            local,
            peer.as_ptr().cast(),
            peer.len(),
            device,
            ciphertext.as_ptr(),
            ciphertext.len(),
            u8::from(pre_key),
            &mut out.0,
        )
    };
    ctx.result(r)?;
    out.bytes()
}
