# VS Code Marketplace Publishing Guide

This guide walks you through publishing the sqry VS Code extension to the Visual Studio Code Marketplace.

## Release Contract

The shipped extension does not trust arbitrary binary downloads. Marketplace and
Open VSX installs autodownload the `sqry` binary only from the public release
repository:

- `https://github.com/verivus-oss/sqry/releases`

That autodownload path verifies:

1. the artifact checksum
2. the Sigstore/Cosign bundle
3. the GitHub Actions OIDC signing identity for the public workflow ref

The expected signing identity is:

```text
https://github.com/verivus-oss/sqry/.github/workflows/release-distribute.yml@refs/tags/v<version>
```

Older public releases may still carry the retired
`oss-distribute.yml` workflow identity; the extension accepts that
identity only as a legacy fallback.

For both current and legacy workflow names, the extension also accepts
the corresponding `@refs/heads/main` identity as a compatibility fallback
for historical public release artifacts that were signed from a
main-branch dispatch rather than the tag ref.

Release artifacts must therefore be published from `verivus-oss/sqry`, and the
public release must include:

- `SHA256SUMS.txt`
- the platform binary assets
- `release-artifacts.attestation.json`

The attestation bundle must use the modern Sigstore bundle schema that
`sigstore-js` accepts (`mediaType`, `content`, `verificationMaterial`).

`binaryVersion` sequencing is strict: the matching public release for that
version must already exist on `verivus-oss/sqry` before the marketplace/Open VSX
extension publish is visible to users, otherwise autodownload will fail with a
404 even if the extension itself is packaged correctly.

## Prerequisites

✅ **Already Complete**:
- Extension packaged (ready to build v0.0.8)
- CHANGELOG.md created with version history
- package.json configured with keywords, categories, gallery banner
- TypeScript compilation successful
- ESLint configuration added
- Version set to 0.0.8

## One-Time Setup (15 minutes)

### 1. Create Azure DevOps Account

1. Go to https://dev.azure.com
2. Sign in with your Microsoft account (or create one)
3. Create an organization (e.g., "verivus")

### 2. Generate Personal Access Token (PAT)

1. In Azure DevOps, click on **User Settings** (gear icon) → **Personal Access Tokens**
2. Click **+ New Token**
3. Configure the token:
   - **Name**: `vsce-marketplace-publish`
   - **Organization**: Select your organization (e.g., "verivus")
   - **Expiration**: 90 days or Custom (recommend 1 year)
   - **Scopes**: Select **Custom defined**
     - Check **Marketplace** → **Manage**
4. Click **Create**
5. **IMPORTANT**: Copy the token immediately - you cannot view it again!
   - Save it securely (password manager recommended)

### 3. Create/Register Publisher

Option A - Using vsce (Recommended):
```bash
cd tools/sqry-vscode
npx @vscode/vsce create-publisher verivus
# Follow the prompts to enter publisher details
```

Option B - Web Interface:
1. Go to https://marketplace.visualstudio.com/manage/createpublisher
2. Fill in publisher details:
   - **ID**: `verivus`
   - **Name**: `Verivus`
   - **Email**: your email

### 4. Login to vsce

```bash
cd tools/sqry-vscode
npx @vscode/vsce login verivus
# Enter your PAT when prompted
```

## Publishing v0.0.8 (5 minutes)

### Method 1: Build and Publish

```bash
cd tools/sqry-vscode
npm run compile
npx @vscode/vsce package
npx @vscode/vsce publish --packagePath sqry-vscode-0.0.8.vsix
```

### Method 2: Rebuild and Publish

```bash
cd tools/sqry-vscode

# Ensure everything is compiled
npm run compile

# Publish (automatically packages and uploads)
npx @vscode/vsce publish
```

### Verification

1. Check the Marketplace: https://marketplace.visualstudio.com/items?itemName=verivus.sqry-vscode
2. Extension should appear within 5-10 minutes
3. Test installation:
   ```bash
   code --install-extension verivus.sqry-vscode
   ```
4. Test autodownload from the installed extension on a machine without `sqry`
   already on `PATH`, and confirm the binary is fetched from the public release
   and provenance verification succeeds.

## Future Updates

### Version Bump and Publish

```bash
cd tools/sqry-vscode

# Update CHANGELOG.md with new version changes
# Then bump version and publish in one command:

npx @vscode/vsce publish patch  # 0.0.8 → 0.0.9
# or
npx @vscode/vsce publish minor  # 0.0.8 → 0.1.0
# or
npx @vscode/vsce publish major  # 0.0.8 → 1.0.0
```

This will:
1. Bump version in package.json
2. Compile TypeScript
3. Package the extension
4. Publish to Marketplace

### Manual Version Control

```bash
cd tools/sqry-vscode

# 1. Update CHANGELOG.md
# 2. Bump version manually
npm version patch

# 3. Commit changes
git add .
git commit -m "chore(vscode): bump to v0.0.7"

# 4. Publish
npx @vscode/vsce publish

# 5. Tag the release
git tag vscode-v0.0.7
git push && git push --tags
```

## Recommended Workflow

```bash
cd tools/sqry-vscode

# 1. Make changes to extension code

# 2. Update CHANGELOG.md with new features/fixes

# 3. Compile and test
npm run compile
# Test in VS Code Extension Development Host (F5)

# 4. Publish with automatic version bump
npx @vscode/vsce publish patch

# 5. Commit and tag
git add .
git commit -m "chore(vscode): publish v0.0.X"
git tag vscode-v0.0.X
git push && git push --tags
```

## Package Details

Current v0.0.8 package:
- **Size**: 296.63 KB (151 files)
- **Bundle**: dist/extension.js (364.54 KB minified + source map)
- **Production build**: Webpack minification reduces from 938 KB → 365 KB
- **Node modules**: Only vscode-languageclient (686.62 KB) and which (7.33 KB) bundled
- **Compiled**: TypeScript → JavaScript via webpack + ts-loader

## Optimization (Optional)

The extension currently includes 183 JavaScript files. For better performance, consider bundling with webpack:

```bash
# Add webpack bundling
npm install --save-dev webpack webpack-cli ts-loader

# Update package.json scripts:
# "vscode:prepublish": "npm run compile && webpack --mode production"
```

This can reduce size from 345 KB → ~100 KB and improve activation time.

## Troubleshooting

### "Extension validation failed"
- Check package.json has all required fields
- Ensure icon file exists at media/sqry-icon.png
- Verify repository URL is accessible

### "Authentication failed"
- PAT might have expired - generate a new one
- Re-login: `npx @vscode/vsce login verivus`

### "Publisher not found"
- Create publisher first: `npx @vscode/vsce create-publisher verivus`
- Or register at https://marketplace.visualstudio.com/manage/createpublisher

### "ENOENT: no such file or directory"
- Make sure you're in tools/sqry-vscode directory
- Run `npm run compile` to generate dist/ folder

## References

- [VS Code Publishing Guide](https://code.visualstudio.com/api/working-with-extensions/publishing-extension)
- [vsce Documentation](https://github.com/microsoft/vscode-vsce)
- [Marketplace Management](https://marketplace.visualstudio.com/manage)

## Support

For issues with publishing:
1. Check the [vsce GitHub Issues](https://github.com/microsoft/vscode-vsce/issues)
2. Review [VS Code Extension API docs](https://code.visualstudio.com/api)
3. Contact Microsoft support via Azure DevOps

---

**Status**: Ready to package and test v0.0.8 ✅

Prerequisites complete:
- ✅ ESLint configuration added
- ✅ Version numbers reconciled
- ✅ CHANGELOG updated
- ⏳ Need to build and test VSIX package

Next: `npm run compile && npx @vscode/vsce package`
