pub trait ToNativeEndian {
    fn to_ne(self) -> Self;
}

/**
 * A simple trait that adds a function to u32/u64 types
 * to convert a number's endianness to native-endian
 * based on the platform the code was built for
 */
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
