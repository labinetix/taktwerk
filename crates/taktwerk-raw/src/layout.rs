//! C struct layout: offsets, size and alignment of a flat struct of scalars and pointers.
//!
//! The rules are C's on the target this crate is compiled for: every member starts at the next
//! multiple of its own alignment, the struct is aligned to its widest member and its size is
//! rounded up to that. Widths come from `size_of`/`align_of` of the Rust type with the same C
//! meaning, never from literals, so a 32-bit target would give different and equally correct
//! answers.

use core::ffi::c_void;
use core::mem::{align_of, size_of};

use taktwerk_core::value::ScalarType;

/// How one member is carried in the struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carrier {
    /// `T *`: a data pointer.
    Pointer,
    /// `T`: the scalar itself, inside the struct's bytes.
    Value(ScalarType),
}

impl Carrier {
    /// `(size, align)` in bytes on this target.
    #[must_use]
    pub const fn size_align(self) -> (usize, usize) {
        match self {
            Self::Pointer => (size_of::<*const c_void>(), align_of::<*const c_void>()),
            Self::Value(ty) => scalar_size_align(ty),
        }
    }
}

/// `(size, align)` of one C scalar on this target.
#[must_use]
pub const fn scalar_size_align(ty: ScalarType) -> (usize, usize) {
    match ty {
        ScalarType::F64 => (size_of::<f64>(), align_of::<f64>()),
        ScalarType::F32 => (size_of::<f32>(), align_of::<f32>()),
        ScalarType::I64 => (size_of::<i64>(), align_of::<i64>()),
        ScalarType::I32 => (size_of::<i32>(), align_of::<i32>()),
        ScalarType::I16 => (size_of::<i16>(), align_of::<i16>()),
        ScalarType::I8 => (size_of::<i8>(), align_of::<i8>()),
        ScalarType::U64 => (size_of::<u64>(), align_of::<u64>()),
        ScalarType::U32 => (size_of::<u32>(), align_of::<u32>()),
        ScalarType::U16 => (size_of::<u16>(), align_of::<u16>()),
        ScalarType::U8 => (size_of::<u8>(), align_of::<u8>()),
        ScalarType::Bool => (size_of::<bool>(), align_of::<bool>()),
    }
}

/// One member of a laid-out struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberLayout {
    /// Member name.
    pub name: String,
    /// Pointer, or a scalar by value.
    pub carrier: Carrier,
    /// C's `offsetof`.
    pub offset: usize,
    /// C's `sizeof` of the member.
    pub size: usize,
    /// C's `_Alignof` of the member.
    pub align: usize,
}

impl MemberLayout {
    /// The member's byte range.
    #[must_use]
    pub const fn range(&self) -> core::ops::Range<usize> {
        self.offset..self.offset + self.size
    }
}

/// A flat C struct, laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructLayout {
    members: Vec<MemberLayout>,
    size: usize,
    align: usize,
}

/// A member list that C cannot lay out.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("struct {name}: {detail}")]
pub struct LayoutError {
    /// Struct name.
    pub name: String,
    /// What is wrong.
    pub detail: String,
}

impl StructLayout {
    /// Lay out `members` in declaration order.
    ///
    /// # Errors
    /// An empty member list (C has no empty struct) or an offset beyond the address space.
    pub fn place(name: &str, members: &[(&str, Carrier)]) -> Result<Self, LayoutError> {
        let fail = |detail: String| LayoutError {
            name: name.to_owned(),
            detail,
        };
        if members.is_empty() {
            return Err(fail("no member, and C has no empty struct".to_owned()));
        }
        let mut laid = Vec::with_capacity(members.len());
        let mut cursor = 0_usize;
        let mut struct_align = 1_usize;
        for &(member, carrier) in members {
            let (size, align) = carrier.size_align();
            struct_align = struct_align.max(align);
            cursor = round_up(cursor, align)
                .ok_or_else(|| fail(format!("{member}: offset overflow")))?;
            laid.push(MemberLayout {
                name: member.to_owned(),
                carrier,
                offset: cursor,
                size,
                align,
            });
            cursor = cursor
                .checked_add(size)
                .ok_or_else(|| fail(format!("{member}: size overflow")))?;
        }
        let size =
            round_up(cursor, struct_align).ok_or_else(|| fail("size overflow".to_owned()))?;
        Ok(Self {
            members: laid,
            size,
            align: struct_align,
        })
    }

    /// C's `sizeof`, trailing padding included.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }

    /// C's `_Alignof`: the widest member's alignment.
    #[must_use]
    pub const fn align(&self) -> usize {
        self.align
    }

    /// Every member, in declaration order.
    #[must_use]
    pub fn members(&self) -> &[MemberLayout] {
        &self.members
    }
}

/// The next multiple of `align` at or above `value`.
fn round_up(value: usize, align: usize) -> Option<usize> {
    let align = align.max(1);
    value.div_ceil(align).checked_mul(align)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_int_between_two_pointers_takes_a_word() {
        let layout = StructLayout::place(
            "s",
            &[
                ("p", Carrier::Pointer),
                ("n", Carrier::Value(ScalarType::I32)),
                ("q", Carrier::Pointer),
            ],
        )
        .unwrap();
        let offsets: Vec<usize> = layout.members().iter().map(|m| m.offset).collect();
        assert_eq!(offsets, vec![0, 8, 16]);
        assert_eq!(layout.size(), 24);
        assert_eq!(layout.align(), 8);
    }

    #[test]
    fn small_scalars_pack_and_the_tail_is_padded() {
        let layout = StructLayout::place(
            "s",
            &[
                ("a", Carrier::Value(ScalarType::U8)),
                ("b", Carrier::Value(ScalarType::I16)),
                ("c", Carrier::Value(ScalarType::F32)),
                ("d", Carrier::Value(ScalarType::F64)),
                ("e", Carrier::Value(ScalarType::Bool)),
            ],
        )
        .unwrap();
        let offsets: Vec<usize> = layout.members().iter().map(|m| m.offset).collect();
        assert_eq!(offsets, vec![0, 2, 4, 8, 16]);
        assert_eq!(layout.size(), 24);
    }

    #[test]
    fn an_empty_struct_is_refused() {
        assert!(StructLayout::place("s", &[]).is_err());
    }
}
