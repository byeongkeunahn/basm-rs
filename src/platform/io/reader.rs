use alloc::string::String;
use core::mem::MaybeUninit;
use core::str::FromStr;
use crate::platform::services;

pub struct Reader<const N: usize = { super::DEFAULT_BUF_SIZE }> {
    buf: [MaybeUninit<u8>; N],
    len: usize,
    off: usize,
}

impl<const N: usize> Default for Reader<N> {
    fn default() -> Self {
        Self::new()
    }
}

mod position {
    #[target_feature(enable = "avx2")]
    pub unsafe fn white(s: &[u8]) -> Option<usize> {
        s.iter().position(|&c| c <= b' ')
    }
    #[target_feature(enable = "avx2")]
    pub unsafe fn newline(s: &[u8]) -> Option<usize> {
        s.iter().position(|&c| c == b'\n')
    }
    #[target_feature(enable = "avx2,sse4.2")]
    pub unsafe fn memchr(s: &[u8], delim: u8) -> Option<usize> {
        s.iter().position(|&b| b == delim)
    }
}

impl<const N: usize> Reader<N> {
    const BUF_LEN: usize = N-8;
    const _DUMMY: usize = {
        assert!(N >= super::MIN_BUF_SIZE, "Buffer size for Reader must be at least MIN_BUF_SIZE");
        0
    };
    pub fn new() -> Self {
        Self {
            buf: MaybeUninit::uninit_array(),
            len: 0,
            off: 0
        }
    }
    pub fn try_refill(&mut self, readahead: usize) -> usize {
        /* readahead cannot exceed the buffer size */
        assert!(readahead <= Self::BUF_LEN);
        let end = self.off + readahead;
        if end <= self.len {
            /* data already available */
        } else {
            /* secure space by discarding the already-consumed buffer contents at front */
            if end > Self::BUF_LEN {
                let rem = self.len - self.off;
                unsafe { core::ptr::copy(self.buf.as_ptr().add(self.off), self.buf.as_mut_ptr(), rem); }
                self.len = rem;
                self.off = 0;
            }
            unsafe {
                /* Although the buffer currently falls short of what has been requested,
                * it may still be possible that a full token (which is short)
                * is available within the remains. Thus, we check if we can return
                * without invoking read_stdio. This is crucial for cases where
                * the standard input is a pipe, which includes the local testing
                * console environment. */
                let mut white_pos = self.off;
                while white_pos < self.len {
                    if self.buf[white_pos].assume_init() <= b' ' {
                        break;
                    }
                    white_pos += 1;
                }
                if white_pos == self.len {
                    /* No whitespace has been found. We have to read.
                    * We try to read as much as possible at once. */
                    self.len += services::read_stdio(0, MaybeUninit::slice_assume_init_mut(&mut self.buf[self.len..Self::BUF_LEN]));
                }
                /* Add a null-terminator, whether or not the read was nonsaturating (for SIMD-accelerated unsafe integer read routines).
                This is safe since we spare 8 bytes at the end of the buffer. */
                *self.buf[self.len].assume_init_mut() = 0u8;
            }
        }
        self.len - self.off
    }
    pub fn try_consume(&mut self, bytes: usize) -> usize {
        let mut consumed = 0;
        while consumed < bytes {
            if self.off == self.len && self.try_refill(1) == 0 { break; }
            let delta = core::cmp::min(self.len - self.off, bytes - consumed);
            self.off += delta;
            consumed -= delta;
        }
        consumed
    }
    // We do not use avx2 for this function since most of the time
    // we only skip a few whitespaces.
    pub fn skip_whitespace(&mut self) -> usize {
        let mut len = 0;
        'outer: loop {
            while self.off < self.len {
                if unsafe { self.buf[self.off].assume_init() } > b' ' {
                    break 'outer len;
                }
                self.off += 1;
                len += 1;
            }
            if self.try_refill(1) == 0 { break len; }
        }
    }
    pub fn skip_until_whitespace(&mut self) -> usize {
        let mut len = 0;
        'outer: loop {
            while self.off < self.len {
                if unsafe { self.buf[self.off].assume_init() } <= b' ' {
                    break 'outer len;
                }
                self.off += 1;
                len += 1;
            }
            if self.try_refill(1) == 0 { break len; }
        }
    }
    pub fn until(&mut self, delim: u8, buf: &mut String) -> usize {
        let mut total = 0;
        loop {
            let len = self.len - self.off;
            let range = unsafe { MaybeUninit::slice_assume_init_ref(&self.buf[self.off..self.off + len]) };
            if let Some(i) = unsafe { position::memchr(range, delim) } {
                unsafe { buf.as_mut_vec() }.extend_from_slice(&range[..i]);
                self.off += i + 1;
                break total + i;
            } else {
                unsafe { buf.as_mut_vec() }.extend_from_slice(range);
                self.off = self.len;
                total += len;
                if self.try_refill(1) == 0 { break total; }
            }
        }
    }
    pub fn remain(&self) -> &[u8] {
        unsafe { MaybeUninit::slice_assume_init_ref(&self.buf[self.off..self.len]) }
    }
    pub fn discard(&mut self, until: u8) -> usize {
        let mut len = 0;
        loop {
            let pos = unsafe { position::memchr(self.remain(), until) };
            if let Some(pos) = pos {
                len += pos;
                self.off += pos + 1;
                break len;
            }
            len += self.len - self.off;
            self.off = self.len;
            if self.try_refill(1) == 0 { break len; }
        }
    }

    pub fn ascii(&mut self) -> u8 {
        self.try_refill(1);
        let mut out = 0u8;
        if self.off < self.len {
            out = unsafe { self.buf[self.off].assume_init() };
            self.off += 1;
        }
        out
    }
    pub fn word_buf(&mut self, buf: &mut [u8]) -> usize {
        self.skip_whitespace();
        let mut len = 0;
        while self.off < self.len && len < buf.len() {
            let rem = core::cmp::min(self.len - self.off, buf.len() - len);
            let data = &self.remain()[..rem];
            if let Some(pos) = unsafe { position::white(data) } {
                buf[len..len + pos].copy_from_slice(&data[..pos]);
                len += pos;
                self.off += pos;
                break;
            } else {
                buf[len..len + rem].copy_from_slice(data);
                len += rem;
                self.off += rem;
                self.try_refill(1);
            }
        }
        len
    }
    pub fn word_to_string(&mut self, buf: &mut String) {
        self.skip_whitespace();
        while self.off < self.len {
            let rem = self.len - self.off;
            let data = &self.remain()[..rem];
            if let Some(pos) = unsafe { position::white(data) } {
                unsafe { buf.as_mut_vec() }.extend_from_slice(&data[..pos]);
                self.off += pos;
                break;
            } else {
                unsafe { buf.as_mut_vec() }.extend_from_slice(data);
                self.off += rem;
                self.try_refill(1);
            }
        }
    }
    pub fn word(&mut self) -> String {
        let mut buf = String::new();
        self.word_to_string(&mut buf);
        buf
    }
    pub fn line_to_string(&mut self, buf: &mut String) {
        while self.off < self.len {
            let rem = self.len - self.off;
            let data = &self.remain()[..rem];
            if let Some(pos) = unsafe { position::newline(data) } {
                let pos_out = if pos > 0 && data[pos - 1] == b'\r' { pos - 1 } else { pos };
                unsafe { buf.as_mut_vec() }.extend_from_slice(&data[..pos_out]);
                self.off += pos + 1;
                break;
            } else {
                unsafe { buf.as_mut_vec() }.extend_from_slice(data);
                self.off += rem;
                self.try_refill(1);
            }
        }
    }
    pub fn line(&mut self) -> String {
        let mut buf = String::new();
        self.line_to_string(&mut buf);
        buf
    }

    fn noskip_u64(&mut self) -> u64 {
        const POW10: [u32; 9] = [1, 10, 100, 1_000, 10_000, 100_000, 1_000_000, 10_000_000, 100_000_000];
        let mut out = 0;
        loop {
            let mut c = unsafe { self.buf[self.off..].as_ptr().cast::<u64>().read_unaligned() };
            let m = !c & 0x1010101010101010;
            let len = m.trailing_zeros() >> 3;
            if len == 0 { break out; }
            self.off += len as usize;
            out *= POW10[len as usize] as u64;
            c <<= (8 - len) << 3;
            c = (c & 0x0F0F0F0F0F0F0F0F).wrapping_mul(2561) >> 8;
            c = (c & 0x00FF00FF00FF00FF).wrapping_mul(6553601) >> 16;
            c = (c & 0x0000FFFF0000FFFF).wrapping_mul(42949672960001) >> 32;
            out += c;
        }
    }
    fn noskip_u128(&mut self) -> u128 {
        let mut n = 0;
        while self.off < self.len {
            let b = unsafe { self.buf[self.off].assume_init() };
            if b > 32 {
                n *= 10;
                n += b as u128 & 0x0F;
                self.off += 1;
            } else {
                break;
            }
        }
        n
    }

    pub fn i8(&mut self) -> i8 {
        self.i32() as i8
    }
    pub fn u8(&mut self) -> u8 {
        self.u32() as u8
    }
    pub fn i16(&mut self) -> i16 {
        self.i32() as i16
    }
    pub fn u16(&mut self) -> u16 {
        self.u32() as u16
    }
    pub fn i32(&mut self) -> i32 {
        self.skip_whitespace();
        self.try_refill(17);
        let sign = unsafe { self.buf[self.off].assume_init() } == b'-';
        (if sign {
            self.off += 1;
            self.noskip_u64().wrapping_neg()
        } else {
            self.noskip_u64()
        }) as i32
    }
    pub fn u32(&mut self) -> u32 {
        self.skip_whitespace();
        self.try_refill(16);
        self.noskip_u64() as u32
    }
    pub fn i64(&mut self) -> i64 {
        self.skip_whitespace();
        self.try_refill(25);
        let sign = unsafe { self.buf[self.off].assume_init() } == b'-';
        (if sign {
            self.off += 1;
            self.noskip_u64().wrapping_neg()
        } else {
            self.noskip_u64()
        }) as i64
    }
    pub fn u64(&mut self) -> u64 {
        self.skip_whitespace();
        self.try_refill(24);
        self.noskip_u64()
    }
    pub fn i128(&mut self) -> i128 {
        self.skip_whitespace();
        self.try_refill(41);
        let sign = unsafe { self.buf[self.off].assume_init() } == b'-';
        (if sign {
            self.off += 1;
            self.noskip_u128().wrapping_neg()
        } else {
            self.noskip_u128()
        }) as i128
    }
    pub fn u128(&mut self) -> u128 {
        self.skip_whitespace();
        self.try_refill(40);
        self.noskip_u128()
    }
    #[cfg(target_pointer_width = "32")]
    pub fn isize(&mut self) -> isize {
        self.i32() as isize
    }
    #[cfg(target_pointer_width = "32")]
    pub fn usize(&mut self) -> usize {
        self.u32() as usize
    }
    #[cfg(target_pointer_width = "64")]
    pub fn isize(&mut self) -> isize {
        self.i64() as isize
    }
    #[cfg(target_pointer_width = "64")]
    pub fn usize(&mut self) -> usize {
        self.u64() as usize
    }
    #[cfg(all(not(target_pointer_width = "32"), not(target_pointer_width = "64")))]
    pub fn isize(&mut self) -> isize {
        self.i128() as isize
    }
    #[cfg(all(not(target_pointer_width = "32"), not(target_pointer_width = "64")))]
    pub fn usize(&mut self) -> usize {
        self.u128() as usize
    }
    pub fn f64(&mut self) -> f64 {
        /* For simplicity, we assume the input string is at most 64 bytes.
         * Strings longer than this length are either incorrectly parsed
         * (scientific notations get their exponents truncated) or approximately parsed
         * (decimal notations get their tails truncated yielding approximately
         * correct outputs). */
        self.skip_whitespace();
        self.try_refill(64);
        let mut end = self.off;
        while end < self.len && unsafe { self.buf[end].assume_init() } > b' ' { end += 1; }
        if end == self.off {
            f64::NAN
        } else {
            let s_u8 = unsafe { MaybeUninit::slice_assume_init_ref(&self.buf[self.off..end]) };
            let s = unsafe { core::str::from_utf8_unchecked(s_u8) };
            let out = f64::from_str(s);
            self.skip_until_whitespace();
            if let Ok(ans) = out {
                ans
            } else {
                f64::NAN
            }
        }
    }

    pub fn is_eof(&mut self) -> bool {
        if self.off == self.len { self.try_refill(1); }
        self.off == self.len
    }
    pub fn is_eof_skip_whitespace(&mut self) -> bool {
        self.skip_whitespace();
        self.off == self.len
    }
}

/*
#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn read_numbers() {
        prepare_stdin(b"1234 -56\n-9999.9999\n");
        let mut reader = Reader::<100>::new();

        assert_eq!(reader.next_usize(), 1234);
        assert_eq!(reader.next_i32(), -56);
        assert_eq!(reader.next_f64(), -9999.9999);
    }

    #[test]
    #[ignore]
    fn read_scientifi_notation() {
        prepare_stdin(b"1e1\n1e-1\n");
        let mut reader = Reader::<100>::new();

        assert_eq!(reader.next_f64(), 10.0);
        assert_eq!(reader.next_f64(), 1e-1);
    }

    #[test]
    fn read_word() {
        prepare_stdin(b"Hello World\nBye\n");
        let mut reader = Reader::<100>::new();
        let mut buf = [0; 100];

        let n = reader.next_word(&mut buf);
        assert_eq!(n, 5);
        assert_eq!(&buf[..n], b"Hello");

        let n = reader.next_word(&mut buf);
        assert_eq!(n, 5);
        assert_eq!(&buf[..n], b"World");

        let n = reader.next_word(&mut buf);
        assert_eq!(n, 3);
        assert_eq!(&buf[..n], b"Bye");
    }

    #[test]
    fn next_until() {
        prepare_stdin(b"Hello World\nBye\n");
        let mut reader = Reader::<100>::new();
        let mut buf = [0; 100];

        let n = reader.next_until(&mut buf, b'\n');
        assert_eq!(n, 11);
        assert_eq!(&buf[..n], b"Hello World");

        let n = reader.next_word(&mut buf);
        assert_eq!(n, 3);
        assert_eq!(&buf[..n], b"Bye");
    }

    #[test]
    #[ignore]
    fn read_word_without_terminator() {
        prepare_stdin(b"no-terminator");
        let mut reader = Reader::<100>::new();
        let mut buf = [0; 100];

        let n = reader.next_word(&mut buf);
        assert_eq!(n, 13);
        assert_eq!(&buf[..n], b"no-terminator");
    }

    #[test]
    #[ignore]
    fn read_word_multiple_space_in_between() {
        // This also affects number reading.
        prepare_stdin(b"1 \n5"); // Trailing space in first line
        let mut reader = Reader::<100>::new();
        let mut buf = [0; 100];

        let n = reader.next_word(&mut buf);
        assert_eq!(n, 1);

        let n = reader.next_word(&mut buf);
        assert_eq!(n, 1);
        assert_eq!(&buf[..n], b"b");
    }

    #[test]
    fn skip_white() {
        prepare_stdin(b" \t\x0b\n5\n");
        let mut reader = Reader::<100>::new();
        assert_eq!(reader.skip_white(), 4);
        assert_eq!(reader.next_usize(), 5);
    }

    #[test]
    fn skip_until() {
        prepare_stdin(b"garbage,5\n");
        let mut reader = Reader::<100>::new();
        assert_eq!(reader.skip_until(b','), b"garbage".len());
        assert_eq!(reader.next_usize(), 5);
    }
}
*/