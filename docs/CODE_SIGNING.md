# Code Signing Setup Guide

This guide explains how to set up code signing for Qontinui Runner releases.

## Overview

Code signing is crucial for distributing the application without security warnings. Each platform has its own requirements:

- **Windows**: Authenticode certificate
- **macOS**: Apple Developer certificate + notarization
- **Linux**: GPG signature for AppImage

## GitHub Secrets Required

Configure these secrets in your GitHub repository settings:

### Windows Signing

1. **WINDOWS_CERTIFICATE**: Base64-encoded .pfx certificate
   ```bash
   # Convert .pfx to base64
   base64 -i certificate.pfx | pbcopy  # macOS
   base64 certificate.pfx | clip        # Windows
   ```

2. **WINDOWS_CERTIFICATE_PASSWORD**: Password for the .pfx file

### macOS Signing

1. **APPLE_CERTIFICATE**: Base64-encoded .p12 certificate
   ```bash
   # Export from Keychain Access as .p12, then:
   base64 -i certificate.p12 | pbcopy
   ```

2. **APPLE_CERTIFICATE_PASSWORD**: Password for the .p12 file

3. **APPLE_SIGNING_IDENTITY**: Your signing identity (e.g., "Developer ID Application: Your Name (TEAMID)")

4. **APPLE_ID**: Your Apple ID email

5. **APPLE_PASSWORD**: App-specific password (not your Apple ID password)
   - Generate at https://appleid.apple.com/account/manage
   - Sign in → Security → App-Specific Passwords

6. **APPLE_TEAM_ID**: Your Apple Developer Team ID

### Tauri Updater Signing

1. **TAURI_SIGNING_PRIVATE_KEY**: Private key for update signatures
   ```bash
   # Generate keys
   npm run tauri signer generate -- -w ~/.tauri/qontinui-runner.key

   # The public key will be displayed - save it
   # Convert private key to base64 for GitHub secret
   base64 -i ~/.tauri/qontinui-runner.key | pbcopy
   ```

2. **TAURI_SIGNING_PRIVATE_KEY_PASSWORD**: Password for the private key

### Sentry (Optional)

1. **SENTRY_DSN**: Your Sentry project DSN
2. **SENTRY_AUTH_TOKEN**: Authentication token for source map uploads

## Platform-Specific Instructions

### Windows

#### Getting a Code Signing Certificate

1. Purchase from a Certificate Authority (CA):
   - DigiCert
   - Sectigo (formerly Comodo)
   - GlobalSign

2. Or use a self-signed certificate for testing:
   ```powershell
   New-SelfSignedCertificate -Type Custom -Subject "CN=Qontinui Runner, O=Your Company" -KeyUsage DigitalSignature -FriendlyName "Qontinui Runner" -CertStoreLocation "Cert:\CurrentUser\My"
   ```

#### Export Certificate

1. Open Certificate Manager (certmgr.msc)
2. Navigate to Personal → Certificates
3. Right-click your certificate → All Tasks → Export
4. Export as .pfx with private key

### macOS

#### Enrollment

1. Enroll in Apple Developer Program ($99/year)
2. Create a Developer ID Application certificate

#### Create Certificate

1. Open Xcode → Preferences → Accounts
2. Select your team → Manage Certificates
3. Create "Developer ID Application" certificate
4. Export from Keychain Access as .p12

#### Notarization Setup

1. Create an app-specific password at appleid.apple.com
2. Store credentials:
   ```bash
   xcrun notarytool store-credentials "AC_PASSWORD" \
     --apple-id "your@email.com" \
     --team-id "TEAMID" \
     --password "app-specific-password"
   ```

### Linux

Linux doesn't require paid certificates. AppImage uses GPG signatures:

```bash
# Generate GPG key
gpg --full-generate-key

# Export public key
gpg --export --armor your@email.com > public.asc

# Sign AppImage (done automatically in CI)
./appimagetool-x86_64.AppImage --sign MyApp.AppImage
```

## Testing Code Signing

### Local Testing

1. **Windows**:
   ```powershell
   signtool sign /fd SHA256 /td SHA256 /tr http://timestamp.digicert.com your-app.exe
   signtool verify /pa your-app.exe
   ```

2. **macOS**:
   ```bash
   codesign --force --deep --sign "Developer ID Application: Your Name" YourApp.app
   codesign --verify --verbose YourApp.app
   spctl -a -vvv -t install YourApp.app
   ```

3. **Linux**:
   ```bash
   ./your-app.AppImage --appimage-signature
   ```

## Troubleshooting

### Windows Issues

- **Error: "The file is being used by another process"**
  - Close any antivirus software temporarily
  - Ensure the file isn't running

- **Error: "Certificate not trusted"**
  - Ensure using a certificate from a trusted CA
  - Check certificate hasn't expired

### macOS Issues

- **Error: "Unable to notarize"**
  - Ensure all binaries are signed
  - Check entitlements are correct
  - Verify Developer ID certificate is valid

- **Error: "The app is damaged"**
  - App wasn't notarized properly
  - Re-sign and re-notarize

### GitHub Actions Issues

- **Secret not found**
  - Check secret names match exactly
  - Ensure secrets are set in repository settings

- **Certificate import fails**
  - Verify base64 encoding is correct
  - Check certificate password is correct

## Security Best Practices

1. **Never commit certificates or keys to the repository**
2. **Use GitHub secrets for all sensitive data**
3. **Rotate certificates before expiration**
4. **Keep certificate passwords strong and unique**
5. **Limit access to signing certificates**
6. **Use separate certificates for development and production**

## Certificate Renewal

### Windows
- Renew before expiration (typically 1-3 years)
- Update GitHub secret with new certificate

### macOS
- Renew annually with Developer Program membership
- Generate new certificate if compromised
- Update all GitHub secrets

## Additional Resources

- [Windows Authenticode Signing](https://docs.microsoft.com/en-us/windows/win32/seccrypto/cryptography-tools)
- [Apple Developer - Notarizing macOS Software](https://developer.apple.com/documentation/security/notarizing_macos_software_before_distribution)
- [Tauri Code Signing Guide](https://tauri.app/v1/guides/distribution/sign)
- [AppImage Signing](https://docs.appimage.org/packaging-guide/optional/signatures.html)
