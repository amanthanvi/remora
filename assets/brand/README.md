# Remora Brand Assets

`remora-app-icon-master.png` is the canonical lossless copy of the approved
application-icon artwork. Platform icon files, including Android's adaptive
foreground, are deterministic size variants of this master; do not round its
corners because iOS and Android apply their own icon masks.

`remora-mascot-master.png` is the transparent in-app mascot derivative. It
preserves the approved remora character while removing the underwater
background so the same mascot can be rendered by SwiftUI and Compose.

Platform derivatives live in:

- `apps/ios/Sources/Remora/Assets.xcassets/AppIcon.appiconset/`
- `apps/ios/Sources/Remora/Assets.xcassets/AppIconMac.appiconset/`
- `apps/ios/Sources/Remora/Assets.xcassets/remora_mascot.imageset/`
- `apps/android/app/src/main/res/drawable-nodpi/`
- `apps/android/app/src/main/res/drawable-xxxhdpi/`

Keep the icon and mascot visually identical across platforms. Android's
monochrome launcher resource is the only intentionally single-color variant.
