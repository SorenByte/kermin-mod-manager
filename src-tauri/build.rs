fn main() {
    // We embed a custom Windows application manifest for ONE reason: to declare
    // `requestedExecutionLevel = asInvoker`, which stops Windows' "installer
    // detection" heuristic from forcing a UAC elevation prompt on an exe whose
    // file name contains "installer" (the earlier `os error 740`).
    //
    // IMPORTANT: supplying a custom manifest REPLACES Tauri's default one, so we
    // must reproduce everything that default provides — most critically the
    // dependency on Common-Controls v6. Without it, comctl32 v5 is loaded and
    // the native dialogs crash with "entry point TaskDialogIndirect not found"
    // (STATUS_ENTRYPOINT_NOT_FOUND / 0xc0000139).
    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*" />
    </dependentAssembly>
  </dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{e2011457-1546-43c5-a5fe-008deee3d3f0}" /><!-- Vista -->
      <supportedOS Id="{35138b9a-5d96-4fbd-8e2d-a2440225f93a}" /><!-- 7 -->
      <supportedOS Id="{4a2f28e3-53b9-4441-ba9c-d69d4a4a6e38}" /><!-- 8 -->
      <supportedOS Id="{1f676c76-80e1-4239-95bb-83d0f6d0da78}" /><!-- 8.1 -->
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}" /><!-- 10 & 11 -->
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
</assembly>
"#;

    tauri_build::try_build(
        tauri_build::Attributes::new().windows_attributes(
            tauri_build::WindowsAttributes::new().app_manifest(manifest),
        ),
    )
    .expect("failed to run tauri-build");
}
