//! Updates a Windows installation in place after the player exits.

use anyhow::{Context, Result, ensure};
use reqwest::{
    Url,
    blocking::{Client, Response},
    redirect::Policy,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    env,
    ffi::OsStr,
    fs,
    io::{Read, Write},
    os::windows::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    process::Command,
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0},
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
        UI::WindowsAndMessaging::{MB_ICONERROR, MessageBoxW},
    },
    core::{HSTRING, PCWSTR},
};

const REPO: &str = "v8816/yandex-music-rust";
const API: &str = "https://api.github.com/repos/v8816/yandex-music-rust/releases/latest";
const MAX_EXE: u64 = 64 * 1024 * 1024;
const STAGE_PREFIX: &str = ".yandex-music-update-";
const HELPER: &str = "update-helper.exe";
const CANDIDATE: &str = "next.exe";
const BACKUP: &str = "previous.exe";
const MANIFEST: &str = "update.json";

#[derive(Clone, Debug)]
pub struct Update {
    pub version: String,
    size: u64,
    digest: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

fn version_parts(version: &str) -> Option<[u32; 3]> {
    let parts: Vec<_> = version.split('.').collect();
    let [major, minor, patch] = parts.as_slice() else {
        return None;
    };
    let parse = |s: &str| {
        if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let number: u32 = s.parse().ok()?;
        (s == number.to_string()).then_some(number)
    };
    Some([parse(major)?, parse(minor)?, parse(patch)?])
}

fn asset_name(version: &str) -> String {
    format!("YandexMusicRust-{version}-Windows-x64.exe")
}

fn asset_url(version: &str, filename: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/v{version}/{filename}")
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
}

fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent(concat!("YandexMusicRust/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(150))
        .redirect(Policy::custom(|attempt| {
            let url = attempt.url();
            let github_assets = matches!(
                url.host_str(),
                Some(
                    "release-assets.githubusercontent.com"
                        | "objects.githubusercontent.com"
                        | "github-releases.githubusercontent.com"
                )
            );
            if attempt.previous().len() < 3
                && attempt
                    .previous()
                    .first()
                    .is_some_and(|original| original.host_str() == Some("github.com"))
                && github_assets
                && url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none()
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()?)
}

fn response(client: &Client, url: &str, max_bytes: u64) -> Result<Response> {
    let response = client.get(Url::parse(url)?).send()?.error_for_status()?;
    ensure!(response.status().is_success(), "GitHub не отдал файл");
    ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= max_bytes),
        "Файл обновления слишком большой"
    );
    Ok(response)
}

fn get(client: &Client, url: &str, max_bytes: u64) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    response(client, url, max_bytes)?
        .take(max_bytes + 1)
        .read_to_end(&mut data)?;
    ensure!(
        data.len() as u64 <= max_bytes,
        "Файл обновления слишком большой"
    );
    Ok(data)
}

pub fn check() -> Result<Option<Update>> {
    let client = client()?;
    let release: Release = serde_json::from_slice(&get(&client, API, 512 * 1024)?)?;
    let Some(version) = release.tag_name.strip_prefix('v') else {
        return Ok(None);
    };
    let Some(latest) = version_parts(version) else {
        return Ok(None);
    };
    let current =
        version_parts(env!("CARGO_PKG_VERSION")).context("Не удалось определить версию плеера")?;
    if latest <= current || release.draft || release.prerelease {
        return Ok(None);
    }
    let name = asset_name(version);
    let checksum = format!("{name}.sha256");
    let asset = |name: &str| {
        release.assets.iter().find(|asset| {
            asset.name == name && asset.browser_download_url == asset_url(version, name)
        })
    };
    let executable = asset(&name).context("В релизе нет файла автообновления")?;
    let checksum = asset(&checksum).context("В релизе нет контрольной суммы")?;
    ensure!(
        (1..=MAX_EXE).contains(&executable.size) && (1..=1024).contains(&checksum.size),
        "Некорректный размер файла обновления"
    );
    let digest = executable
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|digest| valid_digest(digest))
        .context("GitHub не предоставил контрольную сумму обновления")?;
    Ok(Some(Update {
        version: version.to_owned(),
        size: executable.size,
        digest: digest.to_ascii_lowercase(),
    }))
}

#[derive(Deserialize, Serialize)]
struct Manifest {
    executable: PathBuf,
    owner_pid: u32,
    digest: String,
    gpu: bool,
}

pub fn install(update: &Update) -> Result<()> {
    let current = env::current_exe()?;
    let name = current.file_name().context("Не найден файл плеера")?;
    let parent = current.parent().context("Не найдена папка плеера")?;
    let stage = tempfile::Builder::new()
        .prefix(STAGE_PREFIX)
        .tempdir_in(parent)
        .context("Нет доступа на запись в папку плеера")?;

    let client = client()?;
    let filename = asset_name(&update.version);
    let checksum = get(
        &client,
        &asset_url(&update.version, &format!("{filename}.sha256")),
        1024,
    )?;
    let checksum = std::str::from_utf8(&checksum)?
        .split_ascii_whitespace()
        .next()
        .context("Контрольная сумма отсутствует")?;
    ensure!(
        checksum.eq_ignore_ascii_case(&update.digest),
        "Контрольные суммы релиза расходятся"
    );
    let mut data = response(&client, &asset_url(&update.version, &filename), update.size)?
        .take(update.size + 1);
    let mut candidate = fs::File::create_new(stage.path().join(CANDIDATE))?;
    let mut prefix = [0; 2];
    data.read_exact(&mut prefix)?;
    candidate.write_all(&prefix)?;
    let rest = std::io::copy(&mut data, &mut candidate)?;
    ensure!(
        rest + 2 == update.size && &prefix == b"MZ",
        "Файл обновления не прошёл проверку"
    );
    candidate.sync_all()?;
    drop(candidate);
    verify_candidate(&stage.path().join(CANDIDATE), &update.digest)?;
    fs::write(
        stage.path().join(MANIFEST),
        serde_json::to_vec(&Manifest {
            executable: name.into(),
            owner_pid: std::process::id(),
            digest: update.digest.clone(),
            gpu: env::args_os().any(|arg| arg == "--gpu"),
        })?,
    )?;
    let helper = stage.path().join(HELPER);
    fs::copy(&current, &helper)?;
    Command::new(helper)
        .arg("--apply-update")
        .spawn()
        .context("Не удалось запустить помощник обновления")?;
    let _ = stage.keep();
    Ok(())
}

fn wait_for_process(pid: u32) -> Result<()> {
    let process = match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        Ok(process) => process,
        Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let finished = unsafe { WaitForSingleObject(process, 30_000) };
    let _ = unsafe { CloseHandle(process) };
    ensure!(
        finished == WAIT_OBJECT_0,
        "Не удалось дождаться закрытия плеера"
    );
    Ok(())
}

fn verify_candidate(path: &Path, digest: &str) -> Result<()> {
    let mut data = fs::File::open(path)?.take(MAX_EXE + 1);
    let mut hasher = Sha256::new();
    let len = std::io::copy(&mut data, &mut hasher)?;
    ensure!(
        len <= MAX_EXE && format!("{:x}", hasher.finalize()) == digest,
        "Файл обновления не прошёл проверку SHA-256"
    );
    Ok(())
}

fn apply() -> Result<()> {
    let current = env::current_exe()?;
    let stage = current.parent().context("Не найдена папка обновления")?;
    ensure!(
        current.file_name() == Some(OsStr::new(HELPER)) && is_stage(stage),
        "Некорректная папка обновления"
    );
    let manifest: Manifest = serde_json::from_slice(&fs::read(stage.join(MANIFEST))?)?;
    ensure!(
        matches!(
            manifest
                .executable
                .components()
                .collect::<Vec<_>>()
                .as_slice(),
            [Component::Normal(_)]
        ) && manifest
            .executable
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
            && valid_digest(&manifest.digest),
        "Некорректные параметры обновления"
    );
    let parent = stage.parent().context("Не найдена папка плеера")?;
    let target = parent.join(&manifest.executable);
    let candidate = stage.join(CANDIDATE);
    verify_candidate(&candidate, &manifest.digest)?;
    wait_for_process(manifest.owner_pid)?;
    let backup = stage.join(BACKUP);
    let start = || {
        let mut command = Command::new(&target);
        if manifest.gpu {
            command.arg("--gpu");
        }
        command
            .arg("--finish-update")
            .arg(stage.file_name().expect("verified stage folder"))
            .arg(std::process::id().to_string());
        command.spawn()
    };
    if let Err(error) = fs::copy(&target, &backup) {
        let _ = start();
        return Err(error).context("Не удалось сохранить текущую версию");
    }
    if let Err(error) = fs::rename(&candidate, &target) {
        let _ = start();
        return Err(error).context("Не удалось заменить файл плеера");
    }
    if let Err(error) = start() {
        fs::rename(&backup, &target).with_context(|| {
            format!(
                "Не удалось восстановить прежний файл из {}",
                backup.display()
            )
        })?;
        let _ = start();
        return Err(error).context("Не удалось запустить обновлённый плеер");
    }
    Ok(())
}

fn is_stage(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_prefix(STAGE_PREFIX))
        .is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

pub fn handle_startup() -> Result<bool> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--apply-update") {
        if let Err(error) = apply() {
            let message = HSTRING::from(format!("Не удалось установить обновление.\n{error:#}"));
            let title = HSTRING::from("Яндекс Музыка · Rust");
            unsafe {
                MessageBoxW(
                    None,
                    PCWSTR(message.as_ptr()),
                    PCWSTR(title.as_ptr()),
                    MB_ICONERROR,
                );
            }
        }
        return Ok(true);
    }
    if let Some([_, folder, pid]) = args.windows(3).find(|args| args[0] == "--finish-update") {
        let folder = Path::new(folder);
        let pid = pid.to_str().and_then(|value| value.parse().ok());
        if matches!(
            folder.components().collect::<Vec<_>>().as_slice(),
            [Component::Normal(_)]
        ) && is_stage(folder)
            && let Some(pid) = pid
            && wait_for_process(pid).is_ok()
            && let Ok(current) = env::current_exe()
            && let Some(parent) = current.parent()
        {
            let stage = parent.join(folder);
            // Refuse a junction or another reparse point during cleanup.
            if !fs::symlink_metadata(&stage)
                .is_ok_and(|metadata| metadata.is_dir() && metadata.file_attributes() & 0x400 == 0)
            {
                return Ok(false);
            }
            for name in [HELPER, MANIFEST, CANDIDATE, BACKUP] {
                let _ = fs::remove_file(stage.join(name));
            }
            let _ = fs::remove_dir(stage);
        }
    }
    Ok(false)
}
