// Test fixture: Grouped imports with `self`, nested groups, and aliases
// Tests:
// - `use std::io::{self, Read, Write as IoWrite};` (self import + alias)
// - `use mymod::{self, inner::{Thing, Other}, util as util_alias};` (nested group + alias)
// - `use std::collections::{self as collections, HashMap};` (self-as alias)

use std::io::{self, Read, Write as IoWrite};
use mymod::{self, inner::{Thing, Other}, util as util_alias};
use std::collections::{self as collections, HashMap};

