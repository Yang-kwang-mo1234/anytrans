# AnyTrans

내 Mac, iPhone, iPad, Windows PC 사이에서 파일과 폴더를 주고받는 프로그램입니다.
같은 Wi-Fi에서는 기기끼리 직접 연결하고, 밖에서는 셀룰러로도 전송됩니다.
계정이나 자체 서버 없이, 한 번 연결(페어링)한 내 기기끼리만 주고받습니다.

- **코어**: Rust, [iroh](https://www.iroh.computer) (QUIC 기반 P2P)
- **앱**: Tauri 2 + Svelte, 데스크톱과 iOS가 하나의 코드베이스
- **CLI**: macOS·Windows용 `anytrans` 명령

## 기능

- **페어링**: 한 기기가 일회용 코드(QR 포함)를 보여주고, 다른 기기가 스캔하거나 붙여 넣어 연결합니다. 코드는 5분 동안 한 번만 쓸 수 있습니다.
- **파일·폴더 전송**: 폴더는 구조를 유지한 채 보내며, 빈 폴더도 포함됩니다.
- **이어받기**: 전송이 끊기면 받은 지점부터 다시 받습니다. 상대 기기에 닿지 못해 포기한 전송은 앱을 다시 켜면 이어서 보냅니다.
- **무결성 확인**: 파일마다 BLAKE3 해시로 확인하고, 다르면 그 파일만 다시 받습니다.
- **연결 경로 표시**: 로컬 네트워크, 직접 연결, 릴레이 경유 중 무엇으로 전송 중인지 보여줍니다.
- **데스크톱**: 끌어다 놓기로 보내기, 트레이 상주(창을 닫아도 계속 수신), 로그인 시 자동 실행.
- **iOS·iPadOS**: 공유 시트에서 바로 보내기, 사진 보관함에서 보내기, 받은 파일을 파일 앱에 표시, 받은 사진·동영상을 사진 보관함에 저장, 셀룰러에서 큰 파일을 보내기 전 확인.

## 설치

[Releases](https://github.com/Yang-kwang-mo1234/anytrans/releases) 페이지의 최신 버전에서 운영체제에 맞는 파일을 내려받으세요.

| 운영체제 | 파일 | 비고 |
|---|---|---|
| macOS | `AnyTrans_<버전>_universal.dmg` | Apple silicon·Intel Mac 공용 |
| Windows 10·11 (64비트) | `AnyTrans_<버전>_x64-setup.exe` | 일반 설치 프로그램 |
| Windows 10·11 (64비트) | `AnyTrans_<버전>_x64_en-US.msi` | 회사 PC 일괄 배포용. 둘 중 하나만 설치하세요. |
| iPhone, iPad | 제공하지 않음 | 아래 "iPhone·iPad" 참고 |
| CLI (macOS) | `anytrans-cli-macos-universal` | 앱 없이 명령어로만 쓸 때 |
| CLI (Windows) | `anytrans-cli-windows-x64.exe` | 앱 없이 명령어로만 쓸 때 |

설치 파일은 개발자 인증서로 서명되어 있지 않아서, 처음 실행할 때 운영체제가 경고를 띄웁니다. 아래 순서대로 한 번만 허용하면 됩니다.

### macOS

1. `.dmg`를 열고 AnyTrans를 응용 프로그램 폴더로 끌어다 놓습니다.
2. 응용 프로그램 폴더에서 AnyTrans를 엽니다. "확인할 수 없습니다"라는 경고가 뜨면 **완료**를 누릅니다.
3. **시스템 설정 → 개인정보 보호 및 보안**으로 가서, 아래쪽 AnyTrans 항목의 **그래도 열기**를 누릅니다.
4. 다시 AnyTrans를 열고, 묻는 창에서 **열기**를 누릅니다.

처음 실행하면 "로컬 네트워크의 기기를 찾도록 허용"할지 묻습니다. 허용해야 같은 Wi-Fi의 기기와 직접 연결됩니다.

위 방법이 안 되면 터미널에서 다운로드 표시를 지운 뒤 다시 엽니다.

```bash
xattr -dr com.apple.quarantine /Applications/AnyTrans.app
```

### Windows

1. `x64-setup.exe`를 실행합니다.
2. "Windows의 PC 보호" 창이 뜨면 **추가 정보**를 누르고 **실행**을 누릅니다.
3. 설치가 끝나면 시작 메뉴에서 AnyTrans를 엽니다.

처음 실행할 때 Windows 방화벽이 네트워크 접근을 물으면 **개인 네트워크**를 허용하세요. 같은 Wi-Fi의 기기와 직접 연결하는 데 필요합니다.

화면을 그리는 데 Microsoft Edge WebView2가 필요한데, Windows 10·11에는 대부분 이미 들어 있고 없으면 설치 프로그램이 내려받습니다.

### iPhone·iPad

App Store나 TestFlight로는 아직 배포하지 않습니다. 직접 설치하려면 Mac과 Apple Developer 계정이 필요하며, 소스에서 빌드해 설치합니다([iOS·iPadOS 앱](#iosipados-앱) 참고).

### CLI

macOS:

```bash
chmod +x anytrans-cli-macos-universal
xattr -d com.apple.quarantine anytrans-cli-macos-universal
sudo mv anytrans-cli-macos-universal /usr/local/bin/anytrans
```

Windows는 `anytrans-cli-windows-x64.exe`를 `anytrans.exe`로 이름을 바꿔 원하는 폴더에 두고, 그 폴더를 PATH에 추가하면 됩니다.

### 처음 연결하기

1. 한 기기에서 **기기 추가**를 누르면 QR 코드와 코드 문자열이 나옵니다.
2. 다른 기기에서 **기기 추가 → 코드 입력하기**를 누르고, QR을 스캔하거나 코드를 붙여 넣습니다.
3. 양쪽 목록에 상대 기기가 보이면 끝입니다. 이후에는 기기 카드의 **파일 보내기**를 누르거나 파일을 끌어다 놓으면 됩니다.

## 동작 방식

각 기기는 처음 실행할 때 비밀 키를 만들고, 그 공개키가 기기 ID가 됩니다. 모든 연결은 QUIC/TLS로 암호화되고 서로의 ID를 확인합니다. 페어링한 기기 목록에 없는 ID의 전송은 거부합니다.

연결 경로는 iroh가 고릅니다.

1. 같은 네트워크에 있으면 직접 연결합니다(데스크톱은 mDNS로 서로를 찾습니다).
2. 다른 네트워크면 홀펀칭으로 직접 연결을 시도합니다.
3. 둘 다 안 되면 [n0](https://n0.computer)의 공용 릴레이 서버를 거칩니다. 릴레이를 거쳐도 내용은 종단 간 암호화되어 있습니다.

받는 쪽은 전송 중인 파일을 `<받은 파일 폴더>/.anytrans-partial/` 아래에 쓰고, 해시가 맞으면 제자리로 옮깁니다.

## 저장소 구조

```
crates/anytrans-core/    모든 전송 로직 (페어링, 프로토콜, 이어받기)
crates/anytrans-cli/     anytrans 명령
app/ui/                  Svelte 화면
app/src-tauri/           Tauri 앱 셸 (코어를 UI에 연결)
app/src-tauri/gen/apple/ Xcode 프로젝트와 공유 시트 확장(ShareExtension)
app/plugins/ios-bridge/  iOS 네이티브 기능용 Swift 플러그인
```

## 빌드

### 필요한 것

- Rust stable
- Node.js 22 이상과 npm (앱을 빌드할 때)
- iOS: Xcode, rustup, xcodegen, CocoaPods

### CLI

```bash
cargo build --release -p anytrans-cli
```

결과물은 `target/release/anytrans`(Windows는 `anytrans.exe`)입니다.

### 데스크톱 앱 (macOS, Windows)

```bash
cd app
npm ci
npm run tauri dev     # 개발 모드로 실행
npm run tauri build   # 설치 파일 생성
```

설치 파일은 `target/release/bundle/` 아래에 생깁니다.

### iOS·iPadOS 앱

iOS 타깃은 rustup으로 설치해야 합니다. Homebrew의 `rust` 패키지와 함께 쓴다면 rustup의 경로가 먼저 오게 하세요.

```bash
brew install rustup xcodegen cocoapods
export PATH="$(brew --prefix rustup)/bin:$PATH"
rustup default stable
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
```

시뮬레이터용으로 빌드하고 설치합니다.

```bash
cd app
npm ci
npx tauri ios build --debug --target aarch64-sim
xcrun simctl install booted src-tauri/gen/apple/build/arm64-sim/AnyTrans.app
```

`gen/apple/project.yml`을 고쳤다면 `gen/apple`에서 `xcodegen generate`로 Xcode 프로젝트를 다시 만드세요.

실기기에 설치하려면 Apple Developer 계정이 필요합니다. `app/src-tauri/tauri.conf.json`의 `bundle.iOS.developmentTeam`에 팀 ID를 넣고, 번들 ID(`com.anytrans.client`, `com.anytrans.client.share`)와 앱 그룹(`group.com.anytrans.client`)을 계정에 등록해야 합니다.

## CLI 사용법

처음 한 번, 두 기기를 페어링합니다.

```bash
# 기기 A: 코드를 보여주고 기다림
anytrans pair

# 기기 B: A에 표시된 코드로 연결
anytrans pair <코드>
```

그다음부터는 이름이나 ID 앞부분으로 보냅니다.

```bash
anytrans devices                          # 페어링한 기기 목록
anytrans send 사진.jpg 문서폴더 --to "Jerry의 PC"
anytrans receive                          # 켜 두고 받기 (Ctrl-C로 종료)
anytrans forget "Jerry의 PC"              # 페어링 해제
```

모든 명령에 붙일 수 있는 옵션:

| 옵션 | 설명 |
|---|---|
| `--name <이름>` | 다른 기기에 보이는 이 기기의 이름 (한 번 지정하면 기억) |
| `--dir <폴더>` | 받은 파일을 저장할 폴더 |
| `--data-dir <폴더>` | 기기 키와 페어링 목록을 둘 폴더 |
| `--relay <URL>` | 공용 릴레이 대신 쓸 릴레이 서버 |

데스크톱 앱과 CLI는 같은 데이터 폴더를 쓰므로 같은 기기로 취급됩니다. 그래서 둘을 동시에 실행할 수는 없습니다.

## 데이터 위치

| 내용 | macOS | Windows | iOS |
|---|---|---|---|
| 기기 키, 페어링 목록, 설정 | `~/Library/Application Support/anytrans/` | `%APPDATA%\anytrans\` | 앱 데이터 폴더 |
| 받은 파일 | `~/Downloads/AnyTrans/` | `%USERPROFILE%\Downloads\AnyTrans\` | 파일 앱의 "AnyTrans" |

데스크톱 앱에서는 환경 변수 `ANYTRANS_DATA_DIR`, `ANYTRANS_DOWNLOAD_DIR`로 두 위치를 바꿀 수 있습니다. 받은 파일 위치는 앱 설정에서도 바꿀 수 있고, 설정이 환경 변수보다 우선합니다.

데이터 폴더의 파일:

| 파일 | 내용 |
|---|---|
| `device.key` | 이 기기의 비밀 키. 지우면 다른 기기가 됩니다. |
| `peers.json` | 페어링한 기기 목록 |
| `name` | 이 기기의 이름 |
| `settings.json` | 앱 설정 |
| `outbox.json` | 끝내지 못해 다음에 이어 보낼 전송 |

## 개발

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

통합 테스트(`crates/anytrans-core/tests/transfer.rs`)는 한 프로세스 안에 기기 여러 개를 띄워, 릴레이 없이 페어링·전송·이어받기·거부를 확인합니다.

앱 crate는 빌드된 UI를 내장하므로, `cargo`로 앱을 빌드하기 전에 `app`에서 `npm run build`를 한 번 실행해야 합니다.

GitHub Actions:

- **CI** (`.github/workflows/ci.yml`): `main` 푸시와 PR마다 macOS·Windows에서 fmt, clippy, 테스트를 실행합니다.
- **Release** (`.github/workflows/release.yml`): `v0.1.0` 같은 태그를 푸시하면 macOS(범용)·Windows 설치 파일과 CLI를 빌드해 GitHub 릴리스 초안에 올립니다.

새 버전을 낼 때:

1. `app/src-tauri/tauri.conf.json`과 `Cargo.toml`(`workspace.package`)의 `version`을 올립니다.
2. 커밋하고 같은 버전의 태그를 푸시합니다.

   ```bash
   git tag v0.1.0
   git push origin v0.1.0
   ```

3. 워크플로가 끝나면 [Releases](https://github.com/Yang-kwang-mo1234/anytrans/releases)에 초안이 생깁니다. 위 "설치" 표의 파일이 모두 올라왔는지 확인하고 **Publish release**를 누릅니다.

## 알려진 제한

- **iOS 백그라운드**: iOS는 화면에 없는 앱을 곧 멈추게 하므로, 앱이 열려 있을 때만 전송됩니다. 중단된 전송은 앱을 다시 열면 이어집니다.
- **iOS의 로컬 탐색**: iOS에서는 mDNS를 쓰지 않습니다(Apple이 승인한 특수 권한이 필요). iPhone은 인터넷을 통해 상대를 찾은 뒤 같은 Wi-Fi면 직접 연결로 바뀌므로, 인터넷이 전혀 없는 Wi-Fi에서는 iPhone과 연결되지 않습니다.
- **공용 릴레이**: 속도 제한이 있어 릴레이를 거치면 큰 파일이 느릴 수 있습니다. 자주 그렇다면 [iroh-relay](https://crates.io/crates/iroh-relay)로 직접 띄운 서버를 설정에 넣으세요.
- **코드 서명 없음**: macOS에서는 처음 열 때 우클릭 → 열기가 필요하고, Windows에서는 SmartScreen 경고가 뜹니다.
- **`libc` 버전 고정**: `libc` 0.2.190 이상에서는 iroh가 쓰는 `netdev`가 iOS용으로 컴파일되지 않아 0.2.189 이하로 고정했습니다(`app/src-tauri/Cargo.toml`). `netdev`가 고쳐지면 풀어도 됩니다.
- **App Store**: iroh의 의존성이 iOS에서 비공개로 분류된 네트워크 API를 참조하므로, App Store 심사에서 거절될 수 있습니다. 개인 기기 설치(개발용 서명, TestFlight)는 문제없습니다.
- **기기 이름 변경**: 이미 페어링한 기기에는 다시 페어링해야 새 이름이 보입니다.

## 라이선스

MIT
