<#
.SYNOPSIS
    Installs everything Motion Spells needs to build and flash, on a fresh
    Windows machine.

.DESCRIPTION
    Three boards, three toolchains: the wand is Xtensa Rust (espup + espflash),
    the controller is Cortex-M Rust (stable + probe-rs), and the radio bridge is
    Arduino C++ (arduino-cli + the ESP8266 core). The laptop-side manager needs
    Python. This installs all of it, in that order, and is safe to re-run - each
    step checks for what it installs and skips it if it is already there.

    Nothing here needs an elevated shell except the Visual Studio Build Tools,
    which raise their own UAC prompt when they run.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\install.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\install.ps1 -SkipArduino
#>
[CmdletBinding()]
param(
    [switch]$SkipMsvc,
    [switch]$SkipRust,
    [switch]$SkipXtensa,
    [switch]$SkipArduino,
    [switch]$SkipPython,
    # Do not ask for router credentials; just leave .env as the template copy.
    [switch]$NoEnvPrompt
)

$ErrorActionPreference = 'Stop'
# Invoke-WebRequest on PowerShell 5.1 spends most of its time drawing a
# progress bar; turning it off makes downloads several times faster.
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$RepoRoot  = Split-Path -Parent $PSScriptRoot
$ToolsHome = Join-Path $env:LOCALAPPDATA 'motion-spells'
$Downloads = Join-Path $ToolsHome 'downloads'
$BinDir    = Join-Path $ToolsHome 'bin'

$script:Results = New-Object System.Collections.ArrayList

# --- output helpers --------------------------------------------------------

function Write-Step  ($m) { Write-Host ""; Write-Host "==> $m" -ForegroundColor Cyan }
function Write-Ok    ($m) { Write-Host "    [ok]   $m" -ForegroundColor Green }
function Write-Note  ($m) { Write-Host "    ...    $m" -ForegroundColor DarkGray }
function Write-Warn2 ($m) { Write-Host "    [warn] $m" -ForegroundColor Yellow }
function Write-Fail  ($m) { Write-Host "    [FAIL] $m" -ForegroundColor Red }

function Add-Result ($Component, $State, $Detail) {
    [void]$script:Results.Add([pscustomobject]@{
        Component = $Component; State = $State; Detail = $Detail
    })
}

function Test-Command ($Name) {
    $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}

function Get-File ($Url, $Dest) {
    Write-Note "downloading $Url"
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Dest) | Out-Null
    Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing
}

# Adds a directory to PATH for this session and for future ones. Both matter:
# the session so the rest of this script can use what it just installed, the
# user environment so the next terminal still can.
function Add-Path ($Dir) {
    if (-not (Test-Path $Dir)) { return }
    $user = [Environment]::GetEnvironmentVariable('PATH', 'User')
    if ($null -eq $user) { $user = '' }
    $entries = $user -split ';' | Where-Object { $_ -ne '' }
    if ($entries -notcontains $Dir) {
        [Environment]::SetEnvironmentVariable('PATH', (@($Dir) + $entries) -join ';', 'User')
    }
    if (($env:PATH -split ';') -notcontains $Dir) {
        $env:PATH = "$Dir;$env:PATH"
    }
}

# --- 0. preflight ----------------------------------------------------------

Write-Host ""
Write-Host "Motion Spells - Windows setup" -ForegroundColor White
Write-Host "repository: $RepoRoot"
Write-Host "tools:      $ToolsHome"

if ([IntPtr]::Size -ne 8) {
    Write-Fail "This needs 64-bit Windows; the Rust and Arduino toolchains have no 32-bit builds."
    exit 1
}
if ($PSVersionTable.PSVersion.Major -lt 5) {
    Write-Fail "PowerShell 5 or newer required (found $($PSVersionTable.PSVersion))."
    exit 1
}
if (-not (Test-Path (Join-Path $RepoRoot 'wand\Cargo.toml'))) {
    Write-Fail "Run this from inside the repository - wand\Cargo.toml is missing."
    exit 1
}

New-Item -ItemType Directory -Force -Path $Downloads, $BinDir | Out-Null

# --- 1. MSVC build tools ---------------------------------------------------
# Cross-compiling still builds proc-macros and build scripts for the host, and
# those need a host linker. rustup's msvc toolchain expects the Visual Studio
# C++ tools for that; without them every build fails at link time with
# "link.exe not found".

function Test-Msvc {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { return $false }
    $found = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
    return [bool]$found
}

if ($SkipMsvc) {
    Add-Result 'MSVC build tools' 'skipped' '-SkipMsvc'
} else {
    Write-Step "Visual Studio C++ build tools (host linker)"
    if (Test-Msvc) {
        Write-Ok "already installed"
        Add-Result 'MSVC build tools' 'ok' 'already present'
    } else {
        Write-Note "not found - installing. This is the big one: a few GB, and it will ask for admin."
        $installed = $false
        if (Test-Command winget) {
            try {
                $override = '--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
                winget install --id Microsoft.VisualStudio.2022.BuildTools -e --accept-package-agreements --accept-source-agreements --override $override
                $installed = Test-Msvc
            } catch {
                Write-Warn2 "winget install failed: $($_.Exception.Message)"
            }
        }
        if (-not $installed) {
            $bootstrapper = Join-Path $Downloads 'vs_BuildTools.exe'
            Get-File 'https://aka.ms/vs/17/release/vs_BuildTools.exe' $bootstrapper
            Write-Note "running the Visual Studio installer - this takes a while"
            $vsArgs = @('--quiet', '--wait', '--norestart', '--nocache',
                        '--add', 'Microsoft.VisualStudio.Workload.VCTools', '--includeRecommended')
            $p = Start-Process -FilePath $bootstrapper -Wait -PassThru -ArgumentList $vsArgs
            # 3010 is "success, reboot pending", which is fine for our purposes.
            if ($p.ExitCode -ne 0 -and $p.ExitCode -ne 3010) {
                Write-Warn2 "installer exited with $($p.ExitCode)"
            }
            $installed = Test-Msvc
        }
        if ($installed) {
            Write-Ok "installed"
            Add-Result 'MSVC build tools' 'ok' 'installed'
        } else {
            Write-Fail "still missing - Rust builds will fail at link time"
            Add-Result 'MSVC build tools' 'FAILED' 'install manually from aka.ms/vs/17/release/vs_BuildTools.exe, C++ build tools workload'
        }
    }
}

# --- 2. Rust ---------------------------------------------------------------

if ($SkipRust) {
    Add-Result 'Rust' 'skipped' '-SkipRust'
} else {
    Write-Step "Rust (stable, for the controller)"
    Add-Path (Join-Path $env:USERPROFILE '.cargo\bin')

    if (Test-Command rustup) {
        Write-Ok (rustup --version 2>&1 | Select-Object -First 1)
    } else {
        $init = Join-Path $Downloads 'rustup-init.exe'
        Get-File 'https://win.rustup.rs/x86_64' $init
        & $init -y --default-toolchain stable --profile default
        if ($LASTEXITCODE -ne 0) { throw "rustup-init failed ($LASTEXITCODE)" }
        Add-Path (Join-Path $env:USERPROFILE '.cargo\bin')
    }

    if (-not (Test-Command rustup)) {
        Write-Fail "rustup is still not on PATH"
        Add-Result 'Rust' 'FAILED' 'rustup not on PATH'
    } else {
        # The controller's rust-toolchain.toml asks for this target, but adding
        # it now means the first build does not stop to download it.
        Write-Note "adding target thumbv8m.main-none-eabihf"
        rustup target add thumbv8m.main-none-eabihf --toolchain stable | Out-Null
        $rustcVersion = rustc --version 2>&1 | Select-Object -First 1
        Write-Ok $rustcVersion
        Add-Result 'Rust' 'ok' $rustcVersion
    }

    # cargo-binstall pulls the prebuilt release binary for a crate instead of
    # compiling it. espflash, probe-rs and espup take tens of minutes to build
    # from source and seconds to download.
    Write-Step "cargo-binstall (prebuilt binaries for the tools below)"
    if (Test-Command cargo-binstall) {
        Write-Ok "already installed"
    } else {
        try {
            $binstallUrl = 'https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.ps1'
            $binstallScript = (Invoke-WebRequest -UseBasicParsing $binstallUrl).Content
            Invoke-Expression $binstallScript
            Add-Path (Join-Path $env:USERPROFILE '.cargo\bin')
        } catch {
            Write-Warn2 "could not install cargo-binstall: $($_.Exception.Message)"
            Write-Warn2 "falling back to building the tools from source - slower, but it works"
        }
    }
}

function Install-CargoTool ($Crate, $Exe, $Label) {
    Write-Step $Label
    if (Test-Command $Exe) {
        $v = & $Exe --version 2>&1 | Select-Object -First 1
        Write-Ok $v
        Add-Result $Label 'ok' "$v (already present)"
        return
    }
    if (Test-Command cargo-binstall) {
        Write-Note "cargo binstall $Crate"
        cargo binstall -y --locked $Crate
    }
    if (-not (Test-Command $Exe)) {
        Write-Note "building $Crate from source - this takes several minutes"
        cargo install --locked $Crate
    }
    Add-Path (Join-Path $env:USERPROFILE '.cargo\bin')
    if (Test-Command $Exe) {
        $v = & $Exe --version 2>&1 | Select-Object -First 1
        Write-Ok $v
        Add-Result $Label 'ok' $v
    } else {
        Write-Fail "$Exe not available after install"
        Add-Result $Label 'FAILED' "cargo install --locked $Crate"
    }
}

if (-not $SkipRust) {
    Install-CargoTool 'espflash'       'espflash' 'espflash'
    Install-CargoTool 'probe-rs-tools' 'probe-rs' 'probe-rs'
}

# --- 3. Xtensa toolchain ---------------------------------------------------
# The ESP32-S3 core is Xtensa, which upstream Rust does not target. espup
# installs the forked 'esp' toolchain the wand's rust-toolchain.toml asks for,
# plus the LLVM that comes with it.

if ($SkipXtensa) {
    Add-Result 'Xtensa toolchain' 'skipped' '-SkipXtensa'
} else {
    Write-Step "espup + the Xtensa 'esp' toolchain"
    if (-not (Test-Command espup)) {
        Install-CargoTool 'espup' 'espup' 'espup'
    }

    if (-not (Test-Command espup)) {
        Add-Result 'Xtensa toolchain' 'FAILED' 'espup unavailable'
    } else {
        $espInstalled = $null -ne (rustup toolchain list 2>&1 | Select-String -SimpleMatch 'esp')
        if ($espInstalled) {
            Write-Ok "'esp' toolchain already installed"
        } else {
            Write-Note "espup install --targets esp32s3 (downloads a full LLVM + GCC, around 1 GB)"
            espup install --targets esp32s3
        }

        # espup writes the environment it needs to this script. Rather than
        # making every future terminal remember to source it, lift the values
        # into the user environment once.
        $exportScript = Join-Path $env:USERPROFILE 'export-esp.ps1'
        if (Test-Path $exportScript) {
            foreach ($line in Get-Content $exportScript) {
                if ($line -match '^\s*\$Env:PATH\s*=\s*"([^"]*)"\s*\+\s*\$Env:PATH') {
                    Add-Path ($matches[1].TrimEnd(';'))
                }
                elseif ($line -match '^\s*\$Env:([A-Za-z_][A-Za-z0-9_]*)\s*=\s*"([^"]*)"\s*$') {
                    [Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'User')
                    Set-Item -Path "Env:$($matches[1])" -Value $matches[2]
                    Write-Note "$($matches[1]) set permanently"
                }
            }
            Write-Ok "Xtensa environment persisted (no need to source export-esp.ps1)"
        } else {
            Write-Warn2 "espup did not write $exportScript"
        }

        # The wand builds core and alloc from source (build-std), which needs
        # the standard library sources present in the esp toolchain.
        $srcDir = Join-Path $env:USERPROFILE '.rustup\toolchains\esp\lib\rustlib\src'
        if (-not (Test-Path $srcDir)) {
            Write-Note "adding rust-src to the esp toolchain (needed for build-std)"
            rustup component add rust-src --toolchain esp
        }

        if (Test-Path $srcDir) {
            Write-Ok "esp toolchain ready"
            Add-Result 'Xtensa toolchain' 'ok' 'esp channel + rust-src'
        } else {
            Write-Fail "rust-src missing from the esp toolchain"
            Add-Result 'Xtensa toolchain' 'FAILED' 'rustup component add rust-src --toolchain esp'
        }
    }
}

# --- 4. arduino-cli + ESP8266 core -----------------------------------------

if ($SkipArduino) {
    Add-Result 'arduino-cli' 'skipped' '-SkipArduino'
} else {
    Write-Step "arduino-cli + ESP8266 core (for the NodeMCU bridge)"
    Add-Path $BinDir
    if (-not (Test-Command arduino-cli)) {
        $zip = Join-Path $Downloads 'arduino-cli.zip'
        Get-File 'https://downloads.arduino.cc/arduino-cli/arduino-cli_latest_Windows_64bit.zip' $zip
        Expand-Archive -Path $zip -DestinationPath $BinDir -Force
        Add-Path $BinDir
    }

    if (-not (Test-Command arduino-cli)) {
        Write-Fail "arduino-cli not on PATH after install"
        Add-Result 'arduino-cli' 'FAILED' "extract it to $BinDir manually"
    } else {
        Write-Ok (arduino-cli version 2>&1 | Select-Object -First 1)

        # A config file may already exist if the Arduino IDE has been used here.
        arduino-cli config dump 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) { arduino-cli config init | Out-Null }

        $esp8266Index = 'https://arduino.esp8266.com/stable/package_esp8266com_index.json'
        $configDump = arduino-cli config dump 2>&1 | Out-String
        if ($configDump -notmatch [regex]::Escape($esp8266Index)) {
            Write-Note "adding the ESP8266 board index"
            arduino-cli config add board_manager.additional_urls $esp8266Index 2>&1 | Out-Null
            if ($LASTEXITCODE -ne 0) {
                # Older arduino-cli builds have no 'config add'.
                arduino-cli config set board_manager.additional_urls $esp8266Index | Out-Null
            }
        }

        Write-Note "updating the board index"
        arduino-cli core update-index | Out-Null

        $cores = arduino-cli core list 2>&1 | Out-String
        if ($cores -match 'esp8266:esp8266') {
            Write-Ok "esp8266 core already installed"
        } else {
            Write-Note "installing the esp8266 core (around 200 MB)"
            arduino-cli core install esp8266:esp8266
        }

        $cores = arduino-cli core list 2>&1 | Out-String
        if ($cores -match 'esp8266:esp8266') {
            Write-Ok "esp8266 core ready"
            Add-Result 'arduino-cli' 'ok' 'with esp8266 core'
        } else {
            Write-Fail "esp8266 core did not install"
            Add-Result 'arduino-cli' 'FAILED' 'arduino-cli core install esp8266:esp8266'
        }
    }
}

# --- 5. Python + manager dependencies --------------------------------------

if ($SkipPython) {
    Add-Result 'Python' 'skipped' '-SkipPython'
} else {
    Write-Step "Python (for the router manager)"
    if (-not (Test-Command python)) {
        $done = $false
        if (Test-Command winget) {
            try {
                winget install --id Python.Python.3.12 -e --accept-package-agreements --accept-source-agreements
                $done = $true
            } catch {
                Write-Warn2 "winget install failed: $($_.Exception.Message)"
            }
        }
        if (-not $done) {
            $py = Join-Path $Downloads 'python-3.12-amd64.exe'
            Get-File 'https://www.python.org/ftp/python/3.12.10/python-3.12.10-amd64.exe' $py
            Start-Process -FilePath $py -Wait -ArgumentList @('/quiet', 'InstallAllUsers=0', 'PrependPath=1', 'Include_pip=1')
        }
        # The installer edits PATH, but not this already-running shell's copy.
        $env:PATH = [Environment]::GetEnvironmentVariable('PATH', 'User') + ';' + [Environment]::GetEnvironmentVariable('PATH', 'Machine')
    }

    if (-not (Test-Command python)) {
        Write-Fail "python not on PATH - open a new terminal and re-run with -SkipMsvc -SkipRust -SkipXtensa -SkipArduino"
        Add-Result 'Python' 'FAILED' 'not on PATH'
    } else {
        $pyVersion = python --version 2>&1 | Select-Object -First 1
        Write-Ok $pyVersion
        $req = Join-Path $RepoRoot 'controller\manager\requirements.txt'
        Write-Note "installing pyserial and paramiko"
        python -m pip install --disable-pip-version-check --quiet --upgrade pip 2>&1 | Out-Null
        python -m pip install --disable-pip-version-check -r $req
        if ($LASTEXITCODE -eq 0) {
            Write-Ok "manager dependencies installed"
            Add-Result 'Python' 'ok' $pyVersion
        } else {
            # paramiko needs a Python it publishes wheels for; pyserial does not
            # care. Dry-run mode only uses pyserial, so this is not fatal.
            Write-Warn2 "pip failed - most likely paramiko has no wheel for this Python version"
            Write-Warn2 "dry-run mode only needs pyserial: python -m pip install pyserial"
            Add-Result 'Python' 'partial' 'pyserial ok, paramiko may be missing (live SSH only)'
        }
    }
}

# --- 6. .env ---------------------------------------------------------------
# Credentials are gitignored, so they do not travel with the clone. They have to
# be entered on each machine.

Write-Step ".env (router credentials - never committed)"
$envPath     = Join-Path $RepoRoot '.env'
$envTemplate = Join-Path $RepoRoot '.env.example'

if (Test-Path $envPath) {
    Write-Ok ".env already present - leaving it alone"
    Add-Result '.env' 'ok' 'already present'
} else {
    Copy-Item $envTemplate $envPath
    Write-Note "copied .env.example to .env"

    if ($NoEnvPrompt) {
        Write-Warn2 "fill in $envPath before running the manager"
        Add-Result '.env' 'partial' 'template copied, values not filled in'
    } else {
        Write-Host ""
        Write-Host "    Enter the values for this setup. Press Enter to keep the template default." -ForegroundColor DarkGray
        Write-Host "    These are written only to .env, which is gitignored." -ForegroundColor DarkGray

        $ask = @{
            'WIFI_SSID'     = 'WiFi network the router is on (shown on the OLED)'
            'WIFI_PASSWORD' = 'WiFi password'
            'SSH_HOST'      = 'Router address'
            'SSH_USER'      = 'Router SSH user'
            'SSH_PASSWORD'  = 'Router SSH password'
        }
        $updated = foreach ($line in (Get-Content $envPath)) {
            $key = $null
            if ($line -match '^\s*([A-Z_]+)\s*=') { $key = $matches[1] }
            if ($key -and $ask.ContainsKey($key)) {
                $current = ($line -split '=', 2)[1]
                $answer = Read-Host "      $($ask[$key]) [$current]"
                if ([string]::IsNullOrWhiteSpace($answer)) { $line } else { "$key=$answer" }
            } else {
                $line
            }
        }
        Set-Content -Path $envPath -Value $updated -Encoding UTF8
        Write-Ok "written to .env (DRY_RUN is still true - the router is not touched)"
        Add-Result '.env' 'ok' 'filled in, DRY_RUN=true'
    }
}

# --- summary ---------------------------------------------------------------

Write-Host ""
Write-Host "--------------------------------------------------------------" -ForegroundColor White
Write-Host " Setup summary" -ForegroundColor White
Write-Host "--------------------------------------------------------------" -ForegroundColor White
foreach ($r in $script:Results) {
    $colour = switch ($r.State) {
        'ok'      { 'Green' }
        'skipped' { 'DarkGray' }
        'partial' { 'Yellow' }
        default   { 'Red' }
    }
    Write-Host ("  {0,-18} {1,-8} {2}" -f $r.Component, $r.State, $r.Detail) -ForegroundColor $colour
}
Write-Host ""

$failed = @($script:Results | Where-Object { $_.State -eq 'FAILED' })
if ($failed.Count -gt 0) {
    Write-Host "Some components failed. Fix those, then re-run - finished steps are skipped." -ForegroundColor Red
} else {
    Write-Host "Toolchain ready." -ForegroundColor Green
}

Write-Host ""
Write-Host "Next: open a NEW terminal (so the PATH changes take effect), plug in all" -ForegroundColor White
Write-Host "three boards, and run:" -ForegroundColor White
Write-Host ""
Write-Host "    powershell -ExecutionPolicy Bypass -File tools\flash-all.ps1" -ForegroundColor Cyan
Write-Host ""

if ($failed.Count -gt 0) { exit 1 }
