# Unicode Data Provenance

## confusables.txt
- **Source**: https://www.unicode.org/Public/security/15.0.0/confusables.txt
- **Version**: Unicode 15.0.0 (2022-08-26)
- **SHA-256**: `2b10130885c3370b101c52d7baedc452ab7f0e257b86c1e52ee657ecfc29ce64`
- **License**: Unicode License V3 (https://www.unicode.org/license.txt)
- **Copyright**: © 2022 Unicode®, Inc.
- **Usage**: UTS #39 Unicode Security Mechanisms — confusable character mappings

## License Compatibility
Unicode License V3 is listed in deny.toml allowed licenses as "Unicode-3.0".
No additional license exception needed.

## Regenerating data.rs
If `confusables.txt` is updated, regenerate the Rust lookup tables:

```bash
python3 scripts/generate_confusables.py
```

This will update `sqry-nl/src/preprocess/confusables/data.rs`.
