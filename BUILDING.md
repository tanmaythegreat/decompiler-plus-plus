# Building

The command line tool has two dependencies and needs nothing but Rust.
The viewer adds FLTK, which builds from source the first time.

```
cargo build --release                     # command line only
cargo build --release --features gui      # command line + viewer
```

## macOS

FLTK builds against the system frameworks, so there is nothing to install
beyond the compiler toolchain:

```
xcode-select --install          # if you have never built C on this machine
brew install cmake              # FLTK's build system
cargo build --release --features gui
./target/release/dpp-gui ./bin/testing_O0 main
```

The first build takes a few minutes while FLTK compiles; after that it is
cached. Apple silicon and Intel both work — nothing in the decompiler assumes
it is running on the architecture it is analysing, so an arm64 Mac reads
x86-64 binaries perfectly well.

If the window opens behind the terminal, that is macOS being cautious about
focus for unsigned binaries; clicking the icon in the Dock brings it forward.
To make it a proper `.app` that opens in front and shows up in the Dock with a
name:

```
mkdir -p dpp.app/Contents/MacOS
cp target/release/dpp-gui dpp.app/Contents/MacOS/
cat > dpp.app/Contents/Info.plist <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>decompiler++</string>
  <key>CFBundleExecutable</key><string>dpp-gui</string>
  <key>CFBundleIdentifier</key><string>local.decompiler-plus-plus</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
open dpp.app
```

## Linux

FLTK needs the X11 and font development headers:

```
sudo apt install cmake g++ libx11-dev libxext-dev libxft-dev \
     libxinerama-dev libxcursor-dev libxrender-dev libxfixes-dev \
     libpango1.0-dev libgl1-mesa-dev
```

Fedora: `cmake gcc-c++ libX11-devel libXext-devel libXft-devel
libXinerama-devel libXcursor-devel libXrender-devel libXfixes-devel
pango-devel mesa-libGL-devel`.

## Windows

```
cargo build --release --features gui
```

Needs the MSVC build tools and CMake. FLTK uses the Win32 API directly, so
there is no runtime to ship — `dpp-gui.exe` is self-contained.
