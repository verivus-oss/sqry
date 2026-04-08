//! Java 9+ module attribute parser (JVMS 4.7.25).
//!
//! Extracts the `Module` attribute from `module-info.class` files and converts
//! the parsed representation into our [`ModuleStub`] model type. This module
//! bridges cafebabe's `ModuleData` to our stub types, converting all internal
//! JVM names (`/`-separated) to fully-qualified names (`.`-separated).
//!
//! The `Module` attribute is only present on `module-info.class` files produced
//! by `javac` for Java 9+ `module-info.java` source files.

// JVM module_info attributes are spec-bounded to u16; casts are intentional
#![allow(clippy::cast_possible_truncation)]

use cafebabe::attributes::AttributeData;

use crate::ClasspathResult;
use crate::stub::model::{
    AccessFlags, ModuleExports, ModuleOpens, ModuleProvides, ModuleRequires, ModuleStub,
};

use super::constants::class_name_to_fqn;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Extract module information from a parsed class file.
///
/// Searches the class-level attributes for a `Module` attribute and converts
/// it into a [`ModuleStub`]. Module names, package names, and class names are
/// all converted from JVM internal form (`/` separator) to FQN form (`.`
/// separator).
///
/// Returns `Ok(None)` if the class file does not contain a `Module` attribute
/// (i.e., it is not a `module-info.class`). Returns an error if the `Module`
/// attribute is present but cannot be converted.
#[allow(clippy::missing_errors_doc)] // Internal helper function
pub fn extract_module(class: &cafebabe::ClassFile<'_>) -> ClasspathResult<Option<ModuleStub>> {
    let module_data = class.attributes.iter().find_map(|attr| match &attr.data {
        AttributeData::Module(data) => Some(data),
        _ => None,
    });

    let Some(data) = module_data else {
        return Ok(None);
    };

    let stub = convert_module_data(data)?;
    Ok(Some(stub))
}

// ---------------------------------------------------------------------------
// Internal conversion
// ---------------------------------------------------------------------------

/// Convert cafebabe's `ModuleData` into our `ModuleStub`.
fn convert_module_data(data: &cafebabe::attributes::ModuleData<'_>) -> ClasspathResult<ModuleStub> {
    let name = class_name_to_fqn(&data.name);
    let access = AccessFlags::new(data.access_flags.bits());
    let version = data.version.as_ref().map(std::string::ToString::to_string);

    let requires = data
        .requires
        .iter()
        .map(convert_requires_entry)
        .collect::<ClasspathResult<Vec<_>>>()?;

    let exports = data
        .exports
        .iter()
        .map(convert_exports_entry)
        .collect::<ClasspathResult<Vec<_>>>()?;

    let opens = data
        .opens
        .iter()
        .map(convert_opens_entry)
        .collect::<ClasspathResult<Vec<_>>>()?;

    let provides = data
        .provides
        .iter()
        .map(convert_provides_entry)
        .collect::<ClasspathResult<Vec<_>>>()?;

    let uses = data
        .uses
        .iter()
        .map(|class_name| class_name_to_fqn(class_name))
        .collect();

    Ok(ModuleStub {
        name,
        access,
        version,
        requires,
        exports,
        opens,
        provides,
        uses,
    })
}

/// Convert a cafebabe `ModuleRequireEntry` to our `ModuleRequires`.
#[allow(clippy::unnecessary_wraps)] // Result for API consistency
fn convert_requires_entry(
    entry: &cafebabe::attributes::ModuleRequireEntry<'_>,
) -> ClasspathResult<ModuleRequires> {
    Ok(ModuleRequires {
        module_name: class_name_to_fqn(&entry.name),
        access: AccessFlags::new(entry.flags.bits()),
        version: entry.version.as_ref().map(std::string::ToString::to_string),
    })
}

/// Convert a cafebabe `ModuleExportsEntry` to our `ModuleExports`.
#[allow(clippy::unnecessary_wraps)] // Result for API consistency
fn convert_exports_entry(
    entry: &cafebabe::attributes::ModuleExportsEntry<'_>,
) -> ClasspathResult<ModuleExports> {
    let to_modules = entry
        .exports_to
        .iter()
        .map(|m| class_name_to_fqn(m))
        .collect();

    Ok(ModuleExports {
        package: class_name_to_fqn(&entry.package_name),
        access: AccessFlags::new(entry.flags.bits()),
        to_modules,
    })
}

/// Convert a cafebabe `ModuleOpensEntry` to our `ModuleOpens`.
#[allow(clippy::unnecessary_wraps)] // Result for API consistency
fn convert_opens_entry(
    entry: &cafebabe::attributes::ModuleOpensEntry<'_>,
) -> ClasspathResult<ModuleOpens> {
    let to_modules = entry
        .opens_to
        .iter()
        .map(|m| class_name_to_fqn(m))
        .collect();

    Ok(ModuleOpens {
        package: class_name_to_fqn(&entry.package_name),
        access: AccessFlags::new(entry.flags.bits()),
        to_modules,
    })
}

/// Convert a cafebabe `ModuleProvidesEntry` to our `ModuleProvides`.
#[allow(clippy::unnecessary_wraps)] // Result for API consistency
fn convert_provides_entry(
    entry: &cafebabe::attributes::ModuleProvidesEntry<'_>,
) -> ClasspathResult<ModuleProvides> {
    let implementations = entry
        .provides_with
        .iter()
        .map(|c| class_name_to_fqn(c))
        .collect();

    Ok(ModuleProvides {
        service: class_name_to_fqn(&entry.service_interface_name),
        implementations,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ClasspathError;
    use cafebabe::ParseOptions;

    // -----------------------------------------------------------------------
    // Module class file builder for tests
    // -----------------------------------------------------------------------

    /// Builds minimal `module-info.class` bytecode with a Module attribute.
    ///
    /// Constructs valid JVM class file bytes containing the necessary constant
    /// pool entries (UTF-8, Class, Module, Package) and a complete Module
    /// attribute with requires, exports, opens, uses, and provides directives.
    struct ModuleBuilder {
        /// Raw constant pool entries (each entry is tag + data bytes).
        cp_entries: Vec<Vec<u8>>,
        /// Constant pool index of the `CONSTANT_Module_info` for this module.
        module_name_idx: u16,
        /// Module-level access flags (`ACC_OPEN`, `ACC_SYNTHETIC`, `ACC_MANDATED`).
        module_flags: u16,
        /// Constant pool index for the module version UTF-8 string (0 = none).
        module_version_idx: u16,
        /// Pending requires directives: (`module_cp_idx`, flags, `version_cp_idx`).
        requires: Vec<(u16, u16, u16)>,
        /// Pending exports directives: (`package_cp_idx`, flags, `to_module_cp_indices`).
        exports: Vec<(u16, u16, Vec<u16>)>,
        /// Pending opens directives: (`package_cp_idx`, flags, `to_module_cp_indices`).
        opens: Vec<(u16, u16, Vec<u16>)>,
        /// Pending uses directives: `class_cp_indices`.
        uses: Vec<u16>,
        /// Pending provides directives: (`service_class_cp_idx`, `impl_class_cp_indices`).
        provides: Vec<(u16, Vec<u16>)>,
    }

    impl ModuleBuilder {
        /// Create a builder for a module with the given name.
        ///
        /// Pre-populates the constant pool with entries needed for the class
        /// file structure (`this_class`, `super_class`) and the Module attribute
        /// name and module name.
        fn new(module_name: &str) -> Self {
            let mut builder = Self {
                cp_entries: Vec::new(),
                module_name_idx: 0,
                module_flags: 0,
                module_version_idx: 0,
                requires: Vec::new(),
                exports: Vec::new(),
                opens: Vec::new(),
                uses: Vec::new(),
                provides: Vec::new(),
            };

            // CP#1: UTF-8 "module-info"
            builder.add_utf8("module-info");
            // CP#2: CONSTANT_Class -> #1
            builder.add_class(1);
            // CP#3: UTF-8 "java/lang/Object"
            builder.add_utf8("java/lang/Object");
            // CP#4: CONSTANT_Class -> #3
            builder.add_class(3);
            // CP#5: UTF-8 "Module"
            builder.add_utf8("Module");
            // CP#6: UTF-8 module_name
            builder.add_utf8(module_name);
            // CP#7: CONSTANT_Module -> #6
            builder.module_name_idx = builder.add_module(6);

            builder
        }

        /// Set module access flags (`ACC_OPEN=0x0020`, `ACC_SYNTHETIC=0x1000`,
        /// `ACC_MANDATED=0x8000`).
        fn module_flags(mut self, flags: u16) -> Self {
            self.module_flags = flags;
            self
        }

        /// Set the module version string.
        fn module_version(mut self, version: &str) -> Self {
            self.module_version_idx = self.add_utf8(version);
            self
        }

        /// Add a `CONSTANT_Utf8` entry. Returns 1-based constant pool index.
        fn add_utf8(&mut self, s: &str) -> u16 {
            let mut entry = vec![1u8]; // tag
            let bytes = s.as_bytes();
            entry.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
            entry.extend_from_slice(bytes);
            self.cp_entries.push(entry);
            self.cp_entries.len() as u16
        }

        /// Add a `CONSTANT_Class` entry. Returns 1-based constant pool index.
        fn add_class(&mut self, name_idx: u16) -> u16 {
            let mut entry = vec![7u8]; // tag
            entry.extend_from_slice(&name_idx.to_be_bytes());
            self.cp_entries.push(entry);
            self.cp_entries.len() as u16
        }

        /// Add a `CONSTANT_Module_info` entry (tag 19). Returns 1-based index.
        fn add_module(&mut self, name_idx: u16) -> u16 {
            let mut entry = vec![19u8]; // tag
            entry.extend_from_slice(&name_idx.to_be_bytes());
            self.cp_entries.push(entry);
            self.cp_entries.len() as u16
        }

        /// Add a `CONSTANT_Package_info` entry (tag 20). Returns 1-based index.
        fn add_package(&mut self, name_idx: u16) -> u16 {
            let mut entry = vec![20u8]; // tag
            entry.extend_from_slice(&name_idx.to_be_bytes());
            self.cp_entries.push(entry);
            self.cp_entries.len() as u16
        }

        /// Add a `requires` directive.
        fn add_requires(
            &mut self,
            module_name: &str,
            flags: u16,
            version: Option<&str>,
        ) -> &mut Self {
            let name_idx = self.add_utf8(module_name);
            let module_idx = self.add_module(name_idx);
            let version_idx = version.map_or(0, |v| self.add_utf8(v));
            self.requires.push((module_idx, flags, version_idx));
            self
        }

        /// Add an `exports` directive.
        fn add_exports(
            &mut self,
            package_name: &str,
            flags: u16,
            to_modules: &[&str],
        ) -> &mut Self {
            let pkg_name_idx = self.add_utf8(package_name);
            let pkg_idx = self.add_package(pkg_name_idx);
            let to_indices: Vec<u16> = to_modules
                .iter()
                .map(|m| {
                    let name_idx = self.add_utf8(m);
                    self.add_module(name_idx)
                })
                .collect();
            self.exports.push((pkg_idx, flags, to_indices));
            self
        }

        /// Add an `opens` directive.
        fn add_opens(&mut self, package_name: &str, flags: u16, to_modules: &[&str]) -> &mut Self {
            let pkg_name_idx = self.add_utf8(package_name);
            let pkg_idx = self.add_package(pkg_name_idx);
            let to_indices: Vec<u16> = to_modules
                .iter()
                .map(|m| {
                    let name_idx = self.add_utf8(m);
                    self.add_module(name_idx)
                })
                .collect();
            self.opens.push((pkg_idx, flags, to_indices));
            self
        }

        /// Add a `uses` directive (service interface consumed).
        fn add_uses(&mut self, class_name: &str) -> &mut Self {
            let name_idx = self.add_utf8(class_name);
            let class_idx = self.add_class(name_idx);
            self.uses.push(class_idx);
            self
        }

        /// Add a `provides` directive (service interface + implementations).
        fn add_provides(&mut self, service_class: &str, impl_classes: &[&str]) -> &mut Self {
            let svc_name_idx = self.add_utf8(service_class);
            let svc_idx = self.add_class(svc_name_idx);
            let impl_indices: Vec<u16> = impl_classes
                .iter()
                .map(|c| {
                    let name_idx = self.add_utf8(c);
                    self.add_class(name_idx)
                })
                .collect();
            self.provides.push((svc_idx, impl_indices));
            self
        }

        /// Serialize the complete class file to bytes.
        fn build(&self) -> Vec<u8> {
            let mut bytes = Vec::new();

            // Magic number
            bytes.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
            // Minor version
            bytes.extend_from_slice(&0u16.to_be_bytes());
            // Major version: 53 (Java 9, the minimum for modules)
            bytes.extend_from_slice(&53u16.to_be_bytes());

            // Constant pool (count = entries + 1)
            let cp_count = self.cp_entries.len() as u16 + 1;
            bytes.extend_from_slice(&cp_count.to_be_bytes());
            for entry in &self.cp_entries {
                bytes.extend_from_slice(entry);
            }

            // Access flags: ACC_MODULE (0x8000)
            bytes.extend_from_slice(&0x8000u16.to_be_bytes());
            // this_class: CP#2 (Class -> "module-info")
            bytes.extend_from_slice(&2u16.to_be_bytes());
            // super_class: 0 (module-info.class has no superclass per JVMS 4.1)
            bytes.extend_from_slice(&0u16.to_be_bytes());
            // interfaces_count: 0
            bytes.extend_from_slice(&0u16.to_be_bytes());
            // fields_count: 0
            bytes.extend_from_slice(&0u16.to_be_bytes());
            // methods_count: 0
            bytes.extend_from_slice(&0u16.to_be_bytes());

            // attributes_count: 1 (the Module attribute)
            bytes.extend_from_slice(&1u16.to_be_bytes());

            // Module attribute
            let attr_data = self.build_module_attr_data();
            // attribute_name_index: CP#5 ("Module")
            bytes.extend_from_slice(&5u16.to_be_bytes());
            // attribute_length
            bytes.extend_from_slice(&(attr_data.len() as u32).to_be_bytes());
            bytes.extend_from_slice(&attr_data);

            bytes
        }

        /// Build the Module attribute data payload (JVMS 4.7.25).
        fn build_module_attr_data(&self) -> Vec<u8> {
            let mut data = Vec::new();

            // module_name_index (CONSTANT_Module_info)
            data.extend_from_slice(&self.module_name_idx.to_be_bytes());
            // module_flags
            data.extend_from_slice(&self.module_flags.to_be_bytes());
            // module_version_index (CONSTANT_Utf8 or 0)
            data.extend_from_slice(&self.module_version_idx.to_be_bytes());

            // requires
            data.extend_from_slice(&(self.requires.len() as u16).to_be_bytes());
            for &(module_idx, flags, version_idx) in &self.requires {
                data.extend_from_slice(&module_idx.to_be_bytes());
                data.extend_from_slice(&flags.to_be_bytes());
                data.extend_from_slice(&version_idx.to_be_bytes());
            }

            // exports
            data.extend_from_slice(&(self.exports.len() as u16).to_be_bytes());
            for (pkg_idx, flags, to_indices) in &self.exports {
                data.extend_from_slice(&pkg_idx.to_be_bytes());
                data.extend_from_slice(&flags.to_be_bytes());
                data.extend_from_slice(&(to_indices.len() as u16).to_be_bytes());
                for idx in to_indices {
                    data.extend_from_slice(&idx.to_be_bytes());
                }
            }

            // opens
            data.extend_from_slice(&(self.opens.len() as u16).to_be_bytes());
            for (pkg_idx, flags, to_indices) in &self.opens {
                data.extend_from_slice(&pkg_idx.to_be_bytes());
                data.extend_from_slice(&flags.to_be_bytes());
                data.extend_from_slice(&(to_indices.len() as u16).to_be_bytes());
                for idx in to_indices {
                    data.extend_from_slice(&idx.to_be_bytes());
                }
            }

            // uses
            data.extend_from_slice(&(self.uses.len() as u16).to_be_bytes());
            for idx in &self.uses {
                data.extend_from_slice(&idx.to_be_bytes());
            }

            // provides
            data.extend_from_slice(&(self.provides.len() as u16).to_be_bytes());
            for (svc_idx, impl_indices) in &self.provides {
                data.extend_from_slice(&svc_idx.to_be_bytes());
                data.extend_from_slice(&(impl_indices.len() as u16).to_be_bytes());
                for idx in impl_indices {
                    data.extend_from_slice(&idx.to_be_bytes());
                }
            }

            data
        }
    }

    /// Helper: parse raw bytes with cafebabe and run `extract_module`.
    fn parse_and_extract(bytes: &[u8]) -> ClasspathResult<Option<ModuleStub>> {
        let mut opts = ParseOptions::default();
        opts.parse_bytecode(false);
        let class_file = cafebabe::parse_class_with_options(bytes, &opts).map_err(|e| {
            ClasspathError::BytecodeParseError {
                class_name: String::from("<test>"),
                reason: e.to_string(),
            }
        })?;
        extract_module(&class_file)
    }

    // -----------------------------------------------------------------------
    // Test 1: java.base-like module with exports
    // -----------------------------------------------------------------------

    #[test]
    fn test_java_base_module_exports() {
        let mut builder = ModuleBuilder::new("java.base");
        builder.add_exports("java/lang", 0, &[]);
        builder.add_exports("java/util", 0, &[]);
        builder.add_requires("java.base", 0x8000, Some("17")); // ACC_MANDATED

        let bytes = builder.build();
        let stub = parse_and_extract(&bytes).unwrap().unwrap();

        assert_eq!(stub.name, "java.base");
        assert_eq!(stub.exports.len(), 2);
        assert_eq!(stub.exports[0].package, "java.lang");
        assert!(stub.exports[0].to_modules.is_empty()); // unqualified export
        assert_eq!(stub.exports[1].package, "java.util");
        assert_eq!(stub.requires.len(), 1);
        assert_eq!(stub.requires[0].module_name, "java.base");
        assert!(stub.requires[0].access.contains(0x8000)); // ACC_MANDATED
        assert_eq!(stub.requires[0].version.as_deref(), Some("17"));
    }

    // -----------------------------------------------------------------------
    // Test 2: requires transitive flag
    // -----------------------------------------------------------------------

    #[test]
    fn test_requires_transitive() {
        let mut builder = ModuleBuilder::new("com.example.app");
        builder.add_requires("java.base", 0x8000, Some("17")); // ACC_MANDATED
        builder.add_requires("java.logging", 0x0020, None); // ACC_TRANSITIVE

        let bytes = builder.build();
        let stub = parse_and_extract(&bytes).unwrap().unwrap();

        assert_eq!(stub.name, "com.example.app");
        assert_eq!(stub.requires.len(), 2);

        let java_base = &stub.requires[0];
        assert_eq!(java_base.module_name, "java.base");
        assert!(java_base.access.contains(0x8000)); // mandated

        let java_logging = &stub.requires[1];
        assert_eq!(java_logging.module_name, "java.logging");
        assert!(java_logging.access.contains(0x0020)); // transitive
        assert!(java_logging.version.is_none());
    }

    // -----------------------------------------------------------------------
    // Test 3: provides with service implementations
    // -----------------------------------------------------------------------

    #[test]
    fn test_provides_service() {
        let mut builder = ModuleBuilder::new("com.example.provider");
        builder.add_provides(
            "com/example/api/Service",
            &[
                "com/example/impl/ServiceImpl",
                "com/example/impl/ServiceImpl2",
            ],
        );

        let bytes = builder.build();
        let stub = parse_and_extract(&bytes).unwrap().unwrap();

        assert_eq!(stub.provides.len(), 1);
        assert_eq!(stub.provides[0].service, "com.example.api.Service");
        assert_eq!(stub.provides[0].implementations.len(), 2);
        assert_eq!(
            stub.provides[0].implementations[0],
            "com.example.impl.ServiceImpl"
        );
        assert_eq!(
            stub.provides[0].implementations[1],
            "com.example.impl.ServiceImpl2"
        );
    }

    // -----------------------------------------------------------------------
    // Test 4: opens for reflection
    // -----------------------------------------------------------------------

    #[test]
    fn test_opens_for_reflection() {
        let mut builder = ModuleBuilder::new("com.example.reflective");
        // Unqualified open (to all modules)
        builder.add_opens("com/example/internal", 0, &[]);
        // Qualified open (to specific modules)
        builder.add_opens(
            "com/example/private",
            0,
            &["com.example.framework", "com.example.test"],
        );

        let bytes = builder.build();
        let stub = parse_and_extract(&bytes).unwrap().unwrap();

        assert_eq!(stub.opens.len(), 2);

        let open_all = &stub.opens[0];
        assert_eq!(open_all.package, "com.example.internal");
        assert!(open_all.to_modules.is_empty());

        let open_qualified = &stub.opens[1];
        assert_eq!(open_qualified.package, "com.example.private");
        assert_eq!(open_qualified.to_modules.len(), 2);
        assert_eq!(open_qualified.to_modules[0], "com.example.framework");
        assert_eq!(open_qualified.to_modules[1], "com.example.test");
    }

    // -----------------------------------------------------------------------
    // Test 5: uses declarations
    // -----------------------------------------------------------------------

    #[test]
    fn test_uses_declarations() {
        let mut builder = ModuleBuilder::new("com.example.consumer");
        builder.add_uses("com/example/api/Service");
        builder.add_uses("java/sql/Driver");

        let bytes = builder.build();
        let stub = parse_and_extract(&bytes).unwrap().unwrap();

        assert_eq!(stub.uses.len(), 2);
        assert_eq!(stub.uses[0], "com.example.api.Service");
        assert_eq!(stub.uses[1], "java.sql.Driver");
    }

    // -----------------------------------------------------------------------
    // Test 6: class without Module attribute returns None
    // -----------------------------------------------------------------------

    #[test]
    fn test_no_module_attribute_returns_none() {
        // Build a minimal regular class file (no Module attribute).
        let mut bytes = Vec::new();

        // Magic
        bytes.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
        // Minor version
        bytes.extend_from_slice(&0u16.to_be_bytes());
        // Major version: 52 (Java 8)
        bytes.extend_from_slice(&52u16.to_be_bytes());

        // Constant pool: 4 entries => cp_count = 5
        bytes.extend_from_slice(&5u16.to_be_bytes());

        // CP#1: UTF-8 "com/example/Foo"
        bytes.push(1);
        let name = b"com/example/Foo";
        bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());
        bytes.extend_from_slice(name);

        // CP#2: CONSTANT_Class -> #1
        bytes.push(7);
        bytes.extend_from_slice(&1u16.to_be_bytes());

        // CP#3: UTF-8 "java/lang/Object"
        bytes.push(1);
        let obj = b"java/lang/Object";
        bytes.extend_from_slice(&(obj.len() as u16).to_be_bytes());
        bytes.extend_from_slice(obj);

        // CP#4: CONSTANT_Class -> #3
        bytes.push(7);
        bytes.extend_from_slice(&3u16.to_be_bytes());

        // Access flags: ACC_PUBLIC | ACC_SUPER
        bytes.extend_from_slice(&0x0021u16.to_be_bytes());
        // this_class: CP#2
        bytes.extend_from_slice(&2u16.to_be_bytes());
        // super_class: CP#4
        bytes.extend_from_slice(&4u16.to_be_bytes());
        // interfaces_count: 0
        bytes.extend_from_slice(&0u16.to_be_bytes());
        // fields_count: 0
        bytes.extend_from_slice(&0u16.to_be_bytes());
        // methods_count: 0
        bytes.extend_from_slice(&0u16.to_be_bytes());
        // attributes_count: 0
        bytes.extend_from_slice(&0u16.to_be_bytes());

        let result = parse_and_extract(&bytes).unwrap();
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // Test 7: module version and ACC_OPEN flag
    // -----------------------------------------------------------------------

    #[test]
    fn test_module_version_and_open_flag() {
        let builder = ModuleBuilder::new("com.example.open")
            .module_flags(0x0020) // ACC_OPEN
            .module_version("1.0.0");

        let bytes = builder.build();
        let stub = parse_and_extract(&bytes).unwrap().unwrap();

        assert_eq!(stub.name, "com.example.open");
        assert!(stub.access.contains(0x0020)); // ACC_OPEN
        assert_eq!(stub.version.as_deref(), Some("1.0.0"));
    }

    // -----------------------------------------------------------------------
    // Test 8: qualified exports (to specific modules)
    // -----------------------------------------------------------------------

    #[test]
    fn test_qualified_exports() {
        let mut builder = ModuleBuilder::new("com.example.lib");
        builder.add_exports(
            "com/example/internal",
            0,
            &["com.example.app", "com.example.test"],
        );

        let bytes = builder.build();
        let stub = parse_and_extract(&bytes).unwrap().unwrap();

        assert_eq!(stub.exports.len(), 1);
        assert_eq!(stub.exports[0].package, "com.example.internal");
        assert_eq!(stub.exports[0].to_modules.len(), 2);
        assert_eq!(stub.exports[0].to_modules[0], "com.example.app");
        assert_eq!(stub.exports[0].to_modules[1], "com.example.test");
    }

    // -----------------------------------------------------------------------
    // Test 9: comprehensive module with all directive types
    // -----------------------------------------------------------------------

    #[test]
    fn test_comprehensive_module() {
        let mut builder = ModuleBuilder::new("com.example.full");
        builder
            .add_requires("java.base", 0x8000, Some("17"))
            .add_requires("java.logging", 0x0020, None)
            .add_exports("com/example/api", 0, &[])
            .add_exports("com/example/spi", 0, &["com.example.impl"])
            .add_opens("com/example/internal", 0, &[])
            .add_uses("com/example/spi/Plugin")
            .add_provides(
                "com/example/spi/Plugin",
                &["com/example/impl/DefaultPlugin"],
            );

        let bytes = builder.build();
        let stub = parse_and_extract(&bytes).unwrap().unwrap();

        assert_eq!(stub.name, "com.example.full");
        assert_eq!(stub.requires.len(), 2);
        assert_eq!(stub.exports.len(), 2);
        assert_eq!(stub.opens.len(), 1);
        assert_eq!(stub.uses.len(), 1);
        assert_eq!(stub.uses[0], "com.example.spi.Plugin");
        assert_eq!(stub.provides.len(), 1);
        assert_eq!(stub.provides[0].service, "com.example.spi.Plugin");
        assert_eq!(
            stub.provides[0].implementations,
            vec!["com.example.impl.DefaultPlugin"]
        );
    }

    // -----------------------------------------------------------------------
    // Test 10: empty module (no directives)
    // -----------------------------------------------------------------------

    #[test]
    fn test_empty_module() {
        let builder = ModuleBuilder::new("com.example.empty");

        let bytes = builder.build();
        let stub = parse_and_extract(&bytes).unwrap().unwrap();

        assert_eq!(stub.name, "com.example.empty");
        assert!(stub.requires.is_empty());
        assert!(stub.exports.is_empty());
        assert!(stub.opens.is_empty());
        assert!(stub.uses.is_empty());
        assert!(stub.provides.is_empty());
        assert!(stub.version.is_none());
    }

    // -----------------------------------------------------------------------
    // Test 11: requires with ACC_STATIC_PHASE
    // -----------------------------------------------------------------------

    #[test]
    fn test_requires_static_phase() {
        let mut builder = ModuleBuilder::new("com.example.compile");
        // ACC_STATIC_PHASE = 0x0040
        builder.add_requires("org.checkerframework.checker.qual", 0x0040, None);

        let bytes = builder.build();
        let stub = parse_and_extract(&bytes).unwrap().unwrap();

        assert_eq!(stub.requires.len(), 1);
        assert_eq!(
            stub.requires[0].module_name,
            "org.checkerframework.checker.qual"
        );
        assert!(stub.requires[0].access.contains(0x0040)); // ACC_STATIC_PHASE
    }
}
