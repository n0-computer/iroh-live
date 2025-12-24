# Building on Windows

Windows builds require MSYS2 with the UCRT64 environment.

## Setup Instructions

### 1. Install MSYS2

Download and install MSYS2 from [https://www.msys2.org/](https://www.msys2.org/)

### 2. Configure VSCode (Optional)

If using VSCode, add the following to your `settings.json` to enable MSYS2 terminals:

```json
"terminal.integrated.profiles.windows": {
    "PowerShell": {
        "source": "PowerShell",
        "icon": "terminal-powershell"
    },
    "Command Prompt": {
        "path": [
            "${env:windir}\\Sysnative\\cmd.exe",
            "${env:windir}\\System32\\cmd.exe"
        ],
        "args": [],
        "icon": "terminal-cmd"
    },
    "bash (MSYS2)": {
        "path": "C:\\msys64\\usr\\bin\\bash.exe",
        "args": [
            "--login",
            "-i"
        ],
        "env": {
            "CHERE_INVOKING": "1"
        }
    },
    "UCRT64": {
        "path": "C:\\msys64\\usr\\bin\\bash.exe",
        "args": [
            "--login",
            "-i"
        ],
        "env": {
            "MSYSTEM": "UCRT64",
            "CHERE_INVOKING": "1"
        }
    },
    "MINGW64": {
        "path": "C:\\msys64\\usr\\bin\\bash.exe",
        "args": [
            "--login",
            "-i"
        ],
        "env": {
            "MSYSTEM": "MINGW64",
            "CHERE_INVOKING": "1"
        }
    }
}
```

### 3. Update Package Database

Run in **UCRT64** terminal (not base MSYS2):

```bash
pacman -Syu
```

### 4. Install Dependencies

Run in the **UCRT64** terminal:

```bash
pacman -S mingw-w64-ucrt-x86_64-rust \
          mingw-w64-ucrt-x86_64-gcc \
          mingw-w64-ucrt-x86_64-pkgconf \
          mingw-w64-ucrt-x86_64-toolchain \
          mingw-w64-ucrt-x86_64-clang \
          mingw-w64-ucrt-x86_64-llvm \
          mingw-w64-ucrt-x86_64-ffmpeg \
          mingw-w64-ucrt-x86_64-pkg-config \
          mingw-w64-ucrt-x86_64-webrtc-audio-processing \
          base-devel \
          autoconf \
          automake \
          libtool \
          --disable-download-timeout
```

### 5. Set Environment Variables

Add to `~/.bashrc`:

```bash
echo 'export LIBCLANG_PATH=/ucrt64/bin' >> ~/.bashrc
echo 'export PKG_CONFIG_PATH=/ucrt64/lib/pkgconfig' >> ~/.bashrc
source ~/.bashrc
```

## Building the Project

After completing the setup, you can build the project as usual:

```bash
cargo build --release
```

