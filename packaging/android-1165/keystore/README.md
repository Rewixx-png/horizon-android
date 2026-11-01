# Horizon 1.16.5 developer keystore

`horizon-1165-dev.jks` is a **development** signing key for the private
`dev.horizon.android1165` APK. It is committed on purpose: every build must
carry the same certificate so a new APK installs over an older one instead of
being rejected with "signature does not match".

- keystore: `horizon-1165-dev.jks` (PKCS12)
- alias: `horizon1165`
- store/key password: `horizon1165`
- certificate: `CN=Horizon 1.16.5 Dev, OU=Horizon, O=Rewixx, C=US`
- key: RSA 2048, SHA384withRSA, valid 2026-11-01 through 2054-03-19 (10000 days)
- SHA-256 fingerprint:
  `C7:13:F9:5E:2F:4A:CF:48:14:FC:F3:35:F3:C3:A0:04:3A:DD:10:C2:31:4B:84:13:30:2F:0D:99:0C:9B:CE:06`
- SHA-1 fingerprint:
  `43:77:C9:CE:BE:61:B4:C6:F5:7E:CF:20:F7:E8:E5:74:F0:1D:E7:9C`

The password is public by design: it protects nothing but this throwaway dev
identity. **Never use this key to sign a public release.** Publishable builds
need a separate, secret-held release keystore that is not in Git.

Do not regenerate, replace or rotate this key while users still have installed
dev builds: Android refuses an update signed with a different certificate.