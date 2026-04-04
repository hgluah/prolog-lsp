use std::{
    fmt::FormattingOptions,
    ops::{Deref, DerefMut},
};

pub struct NoAlternate<T>(T);

macro_rules! formatter {
    ($ty:ident |$opts:pat_param| $expr:expr) => {
        impl<T> $ty<T> {
            #[inline]
            pub fn into_inner(self) -> T {
                self.0
            }
            #[inline]
            pub fn into(self) -> T {
                self.0
            }
        }
        impl<T> From<T> for $ty<T> {
            #[inline]
            fn from(value: T) -> Self {
                Self(value)
            }
        }
        impl<T> Deref for $ty<T> {
            type Target = T;
            #[inline]
            fn deref(&self) -> &T {
                &self.0
            }
        }
        impl<T> DerefMut for $ty<T> {
            #[inline]
            fn deref_mut(&mut self) -> &mut T {
                &mut self.0
            }
        }
        formatter!(@std::fmt::Debug, $ty |$opts| $expr);
        formatter!(@std::fmt::Display, $ty |$opts| $expr);
        formatter!(@std::fmt::Octal, $ty |$opts| $expr);
        formatter!(@std::fmt::Binary, $ty |$opts| $expr);
        formatter!(@std::fmt::LowerHex, $ty |$opts| $expr);
        formatter!(@std::fmt::UpperHex, $ty |$opts| $expr);
        formatter!(@std::fmt::Pointer, $ty |$opts| $expr);
        formatter!(@std::fmt::LowerExp, $ty |$opts| $expr);
        formatter!(@std::fmt::UpperExp, $ty |$opts| $expr);
    };
    (@$trait:path, $ty:ident |$opts:pat_param| $expr:expr) => {
        impl<T: $trait> $trait for $ty<T> {
            #[inline]
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                #[inline]
                fn new_opts($opts: &mut FormattingOptions) -> &mut FormattingOptions {
                    $expr
                }
                self.0.fmt(&mut f.with_options(*new_opts(&mut f.options())))
            }
        }
    };
}
formatter!(NoAlternate | opts | opts.alternate(false));
