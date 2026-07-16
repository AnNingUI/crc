//! Conservative C target layout knowledge for private coroutine contexts.

use std::collections::BTreeMap;

use crate::config::TargetConfig;

/// A queried ABI fact is either exact or conservatively unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutKnowledge<T> {
    Exact(T),
    Unknown(LayoutUnknownReason),
}

impl<T> LayoutKnowledge<T> {
    /// Returns the exact value when the target model proves it.
    #[must_use]
    pub fn exact(&self) -> Option<&T> {
        match self {
            Self::Exact(value) => Some(value),
            Self::Unknown(_) => None,
        }
    }
}

/// Why a C layout query can't provide an exact result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutUnknownReason {
    UnsupportedTarget,
    UnsupportedType(String),
    UnsupportedDeclarator(String),
    PackingEnvironment,
    DependencyUnknown(String),
    ArithmeticOverflow,
}

/// Exact C size and alignment in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeLayout {
    pub size: u64,
    pub align: u64,
}

/// Exact aggregate layout including each field offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateLayout {
    pub size: u64,
    pub align: u64,
    pub offsets: Vec<u64>,
}

/// Reviewed C data model for one configured target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetLayoutModel {
    pub pointer: TypeLayout,
    pub c_long: TypeLayout,
    pub size_t: TypeLayout,
}

impl TargetLayoutModel {
    /// Resolves a reviewed target data model.
    #[must_use]
    pub fn for_target(target: &TargetConfig) -> LayoutKnowledge<Self> {
        let pointer = match target {
            TargetConfig::Wasm32Wasi => scalar(4, 4),
            TargetConfig::WindowsMsvc
            | TargetConfig::WindowsGnu
            | TargetConfig::LinuxGnu
            | TargetConfig::LinuxMusl
            | TargetConfig::Macos => scalar(8, 8),
            TargetConfig::Host => scalar(
                u64::from(std::mem::size_of::<usize>() as u32),
                u64::from(std::mem::align_of::<usize>() as u32),
            ),
            TargetConfig::Custom(_) => {
                return LayoutKnowledge::Unknown(LayoutUnknownReason::UnsupportedTarget);
            }
        };
        let c_long = match target {
            TargetConfig::WindowsMsvc | TargetConfig::WindowsGnu | TargetConfig::Wasm32Wasi => {
                scalar(4, 4)
            }
            TargetConfig::LinuxGnu | TargetConfig::LinuxMusl | TargetConfig::Macos => scalar(8, 8),
            TargetConfig::Host if cfg!(windows) || pointer.size == 4 => scalar(4, 4),
            TargetConfig::Host => scalar(8, 8),
            TargetConfig::Custom(_) => unreachable!("custom target returned above"),
        };
        LayoutKnowledge::Exact(Self {
            pointer,
            c_long,
            size_t: pointer,
        })
    }

    /// Resolves a declarator-aware C type layout.
    #[must_use]
    pub fn type_layout(
        &self,
        c_type: &str,
        generated_types: &BTreeMap<String, TypeLayout>,
    ) -> LayoutKnowledge<TypeLayout> {
        let original = c_type.trim();
        if original.is_empty() {
            return unknown_declarator(original);
        }
        if original.contains("_Atomic")
            || original.contains("struct ")
            || original.contains("union ")
            || original.contains("enum ")
        {
            return unknown_declarator(original);
        }
        let (base, dimensions) = match array_dimensions(original) {
            Ok(value) => value,
            Err(reason) => return LayoutKnowledge::Unknown(reason),
        };
        let base = base.trim();
        let mut layout = if base.contains('*') {
            self.pointer
        } else {
            match self.scalar_or_known_layout(base, generated_types) {
                LayoutKnowledge::Exact(layout) => layout,
                LayoutKnowledge::Unknown(reason) => return LayoutKnowledge::Unknown(reason),
            }
        };
        for count in dimensions {
            let Some(size) = layout.size.checked_mul(count) else {
                return LayoutKnowledge::Unknown(LayoutUnknownReason::ArithmeticOverflow);
            };
            layout.size = size;
        }
        LayoutKnowledge::Exact(layout)
    }

    fn scalar_or_known_layout(
        &self,
        c_type: &str,
        generated_types: &BTreeMap<String, TypeLayout>,
    ) -> LayoutKnowledge<TypeLayout> {
        let normalized = normalize_scalar(c_type);
        if let Some(layout) = generated_types.get(&normalized) {
            return LayoutKnowledge::Exact(*layout);
        }
        let layout = match normalized.as_str() {
            "bool" | "_Bool" | "char" | "signed char" | "unsigned char" | "int8_t" | "uint8_t" => {
                scalar(1, 1)
            }
            "short" | "short int" | "signed short" | "signed short int" | "unsigned short"
            | "unsigned short int" | "int16_t" | "uint16_t" => scalar(2, 2),
            "int" | "signed" | "signed int" | "unsigned" | "unsigned int" | "int32_t"
            | "uint32_t" | "float" | "cr_poll_status" => scalar(4, 4),
            "long" | "long int" | "signed long" | "signed long int" | "unsigned long"
            | "unsigned long int" => self.c_long,
            "long long"
            | "long long int"
            | "signed long long"
            | "signed long long int"
            | "unsigned long long"
            | "unsigned long long int"
            | "int64_t"
            | "uint64_t"
            | "double" => scalar(8, 8),
            "size_t" | "ptrdiff_t" | "intptr_t" | "uintptr_t" => self.size_t,
            "cr_error" => {
                return self.struct_layout([scalar(4, 4), self.pointer]).map_type();
            }
            "cr_cleanup_stack" => {
                return self
                    .struct_layout([self.pointer, self.size_t, self.size_t])
                    .map_type();
            }
            "cr_awaitable" => {
                return self.struct_layout([self.pointer, self.pointer]).map_type();
            }
            "long double" | "void" => {
                return LayoutKnowledge::Unknown(LayoutUnknownReason::UnsupportedType(normalized));
            }
            _ => {
                return LayoutKnowledge::Unknown(if normalized.starts_with("cr_") {
                    LayoutUnknownReason::DependencyUnknown(normalized)
                } else {
                    LayoutUnknownReason::UnsupportedType(normalized)
                });
            }
        };
        LayoutKnowledge::Exact(layout)
    }

    /// Computes exact C struct layout for already known fields.
    #[must_use]
    pub fn struct_layout(
        &self,
        fields: impl IntoIterator<Item = TypeLayout>,
    ) -> LayoutKnowledge<AggregateLayout> {
        let mut size = 0u64;
        let mut align = 1u64;
        let mut offsets = Vec::new();
        for field in fields {
            let Some(offset) = align_up(size, field.align) else {
                return LayoutKnowledge::Unknown(LayoutUnknownReason::ArithmeticOverflow);
            };
            offsets.push(offset);
            let Some(next) = offset.checked_add(field.size) else {
                return LayoutKnowledge::Unknown(LayoutUnknownReason::ArithmeticOverflow);
            };
            size = next;
            align = align.max(field.align);
        }
        let Some(size) = align_up(size, align) else {
            return LayoutKnowledge::Unknown(LayoutUnknownReason::ArithmeticOverflow);
        };
        LayoutKnowledge::Exact(AggregateLayout {
            size,
            align,
            offsets,
        })
    }

    /// Computes exact C union layout for already known members.
    #[must_use]
    pub fn union_layout(
        &self,
        members: impl IntoIterator<Item = TypeLayout>,
    ) -> LayoutKnowledge<TypeLayout> {
        let mut size = 0u64;
        let mut align = 1u64;
        let mut any = false;
        for member in members {
            any = true;
            size = size.max(member.size);
            align = align.max(member.align);
        }
        if !any {
            return LayoutKnowledge::Unknown(LayoutUnknownReason::UnsupportedDeclarator(
                "empty union".to_owned(),
            ));
        }
        let Some(size) = align_up(size, align) else {
            return LayoutKnowledge::Unknown(LayoutUnknownReason::ArithmeticOverflow);
        };
        LayoutKnowledge::Exact(TypeLayout { size, align })
    }
}

trait AggregateKnowledgeExt {
    fn map_type(self) -> LayoutKnowledge<TypeLayout>;
}

impl AggregateKnowledgeExt for LayoutKnowledge<AggregateLayout> {
    fn map_type(self) -> LayoutKnowledge<TypeLayout> {
        match self {
            LayoutKnowledge::Exact(layout) => LayoutKnowledge::Exact(TypeLayout {
                size: layout.size,
                align: layout.align,
            }),
            LayoutKnowledge::Unknown(reason) => LayoutKnowledge::Unknown(reason),
        }
    }
}

const fn scalar(size: u64, align: u64) -> TypeLayout {
    TypeLayout { size, align }
}

fn unknown_declarator(value: &str) -> LayoutKnowledge<TypeLayout> {
    LayoutKnowledge::Unknown(LayoutUnknownReason::UnsupportedDeclarator(value.to_owned()))
}

fn normalize_scalar(c_type: &str) -> String {
    c_type
        .split_whitespace()
        .filter(|word| !matches!(*word, "const" | "volatile" | "restrict"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn array_dimensions(c_type: &str) -> Result<(&str, Vec<u64>), LayoutUnknownReason> {
    let mut base = c_type.trim_end();
    let mut dimensions = Vec::new();
    while base.ends_with(']') {
        let Some(open) = base.rfind('[') else {
            return Err(LayoutUnknownReason::UnsupportedDeclarator(
                c_type.to_owned(),
            ));
        };
        let count = base[open + 1..base.len() - 1].trim();
        if count.is_empty() || !count.chars().all(|character| character.is_ascii_digit()) {
            return Err(LayoutUnknownReason::UnsupportedDeclarator(
                c_type.to_owned(),
            ));
        }
        let count = count
            .parse::<u64>()
            .map_err(|_| LayoutUnknownReason::ArithmeticOverflow)?;
        if count == 0 {
            return Err(LayoutUnknownReason::UnsupportedDeclarator(
                c_type.to_owned(),
            ));
        }
        dimensions.push(count);
        base = base[..open].trim_end();
    }
    if base.contains('[') || base.contains(']') {
        return Err(LayoutUnknownReason::UnsupportedDeclarator(
            c_type.to_owned(),
        ));
    }
    Ok((base, dimensions))
}

fn align_up(value: u64, align: u64) -> Option<u64> {
    if align == 0 || !align.is_power_of_two() {
        return None;
    }
    value
        .checked_add(align - 1)
        .map(|value| value & !(align - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_targets_expose_stable_pointer_and_long_models() {
        let windows = TargetLayoutModel::for_target(&TargetConfig::WindowsMsvc)
            .exact()
            .copied()
            .expect("Windows model");
        assert_eq!(windows.pointer, scalar(8, 8));
        assert_eq!(windows.c_long, scalar(4, 4));

        let linux = TargetLayoutModel::for_target(&TargetConfig::LinuxGnu)
            .exact()
            .copied()
            .expect("Linux model");
        assert_eq!(linux.pointer, scalar(8, 8));
        assert_eq!(linux.c_long, scalar(8, 8));

        let wasm = TargetLayoutModel::for_target(&TargetConfig::Wasm32Wasi)
            .exact()
            .copied()
            .expect("WASI model");
        assert_eq!(wasm.pointer, scalar(4, 4));
        assert_eq!(wasm.c_long, scalar(4, 4));
        assert!(matches!(
            TargetLayoutModel::for_target(&TargetConfig::Custom("unknown".to_owned())),
            LayoutKnowledge::Unknown(LayoutUnknownReason::UnsupportedTarget)
        ));
    }

    #[test]
    fn resolves_scalars_pointers_arrays_and_function_pointers() {
        let model = TargetLayoutModel::for_target(&TargetConfig::Wasm32Wasi)
            .exact()
            .copied()
            .expect("WASI model");
        let known = BTreeMap::new();
        assert_eq!(
            model.type_layout("uint64_t", &known).exact(),
            Some(&scalar(8, 8))
        );
        assert_eq!(
            model.type_layout("const int *", &known).exact(),
            Some(&scalar(4, 4))
        );
        assert_eq!(
            model.type_layout("int (*)(void)", &known).exact(),
            Some(&scalar(4, 4))
        );
        assert_eq!(
            model.type_layout("uint16_t[3][4]", &known).exact(),
            Some(&scalar(24, 2))
        );
    }

    #[test]
    fn rejects_unknown_typedefs_tags_atomics_and_variable_arrays() {
        let model = TargetLayoutModel::for_target(&TargetConfig::Host)
            .exact()
            .copied()
            .expect("host model");
        let known = BTreeMap::new();
        for c_type in [
            "ApplicationValue",
            "struct ApplicationState",
            "_Atomic(int)",
            "int[count]",
            "int[]",
            "int[0]",
            "long double",
        ] {
            assert!(matches!(
                model.type_layout(c_type, &known),
                LayoutKnowledge::Unknown(_)
            ));
        }
    }

    #[test]
    fn computes_struct_padding_union_size_and_generated_dependencies() {
        let model = TargetLayoutModel::for_target(&TargetConfig::LinuxGnu)
            .exact()
            .copied()
            .expect("Linux model");
        let aggregate = model
            .struct_layout([scalar(1, 1), scalar(8, 8), scalar(4, 4)])
            .exact()
            .cloned()
            .expect("exact struct");
        assert_eq!(aggregate.offsets, [0, 8, 16]);
        assert_eq!(aggregate.size, 24);
        assert_eq!(aggregate.align, 8);
        assert_eq!(
            model.union_layout([scalar(3, 1), scalar(8, 8)]).exact(),
            Some(&scalar(8, 8))
        );

        let known = BTreeMap::from([("cr_child_task".to_owned(), scalar(40, 8))]);
        assert_eq!(
            model.type_layout("cr_child_task", &known).exact(),
            Some(&scalar(40, 8))
        );
        assert!(matches!(
            model.type_layout("cr_missing_task", &known),
            LayoutKnowledge::Unknown(LayoutUnknownReason::DependencyUnknown(_))
        ));
    }
}
