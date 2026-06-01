package buildtags

// FlushFilenameLinux is in a file with the suffix `_linux.go` and NO
// header constraint. Per 02_DESIGN §10.3 + 01_SPEC §6.3 AC-T3.8-4 the
// filename-suffix path alone yields cfg_condition = "linux".
func FlushFilenameLinux() {}
