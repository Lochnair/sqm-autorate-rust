pub trait ToNativeEndian {
    fn to_ne(self) -> Self;
}

impl ToNativeEndian for u32 {
    #[cfg(target_endian = "big")]
    fn to_ne(self) -> Self {
        self.to_be()
    }

    #[cfg(target_endian = "little")]
    fn to_ne(self) -> Self {
        self.to_le()
    }
}

impl ToNativeEndian for u64 {
    #[cfg(target_endian = "big")]
    fn to_ne(self) -> Self {
        self.to_be()
    }

    #[cfg(target_endian = "little")]
    fn to_ne(self) -> Self {
        self.to_le()
    }
}
