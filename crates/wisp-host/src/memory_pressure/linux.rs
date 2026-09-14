use std::path::{Component, Path, PathBuf};

use super::{MemoryBytes, MemoryPressureError, MemorySample};

type ReadFile<'a> = dyn Fn(&Path) -> std::io::Result<String> + 'a;

fn invalid(path: &Path, reason: &'static str) -> MemoryPressureError {
    MemoryPressureError::Invalid {
        path: path.into(),
        reason,
    }
}

fn read(read_file: &ReadFile<'_>, path: &Path) -> Result<String, MemoryPressureError> {
    read_file(path).map_err(|source| MemoryPressureError::Read {
        path: path.into(),
        source,
    })
}

pub(super) fn sample(read_file: &ReadFile<'_>) -> Result<MemorySample, MemoryPressureError> {
    let meminfo = Path::new("/proc/meminfo");
    let mut sample = parse_meminfo(&read(read_file, meminfo)?)?;
    let membership_file = Path::new("/proc/self/cgroup");
    let membership = membership(&read(read_file, membership_file)?)?;
    let Some(membership) = membership else {
        return Ok(sample);
    };
    let mountinfo = read(read_file, Path::new("/proc/self/mountinfo"))?;
    let (mount, directory) = locate_cgroup(&membership, &mountinfo)?;
    for ancestor in directory.ancestors() {
        if !ancestor.starts_with(&mount) {
            break;
        }
        let limit_path = ancestor.join("memory.max");
        let limit = match read_file(&limit_path) {
            Ok(value) => value,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound && ancestor == mount => {
                // The global cgroup root has no memory.max; a namespaced root
                // with an enforced limit exposes it like any other cgroup.
                let controllers = read(read_file, &ancestor.join("cgroup.controllers"))?;
                if controllers
                    .split_whitespace()
                    .any(|controller| controller == "memory")
                {
                    continue;
                }
                return Err(MemoryPressureError::Read {
                    path: limit_path,
                    source,
                });
            }
            Err(source) => {
                return Err(MemoryPressureError::Read {
                    path: limit_path,
                    source,
                })
            }
        };
        if limit.trim() == "max" {
            continue;
        }
        let limit = parse_bytes(&limit, &limit_path)?;
        let current_path = ancestor.join("memory.current");
        let current = parse_bytes(&read(read_file, &current_path)?, &current_path)?;
        sample.total.0 = sample.total.0.min(limit.0);
        sample.available.0 = sample.available.0.min(limit.0.saturating_sub(current.0));
    }
    Ok(sample)
}

fn parse_bytes(value: &str, path: &Path) -> Result<MemoryBytes, MemoryPressureError> {
    value
        .trim()
        .parse()
        .map(MemoryBytes)
        .map_err(|_| invalid(path, "expected an unsigned byte count"))
}

fn parse_meminfo(input: &str) -> Result<MemorySample, MemoryPressureError> {
    let path = Path::new("/proc/meminfo");
    let mut total = None;
    let mut available = None;
    for line in input.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let destination = match name {
            "MemTotal" => &mut total,
            "MemAvailable" => &mut available,
            _ => continue,
        };
        if destination.is_some() {
            return Err(invalid(path, "duplicate memory quantity"));
        }
        let mut words = value.split_whitespace();
        let kib = parse_bytes(
            words
                .next()
                .ok_or_else(|| invalid(path, "missing memory quantity"))?,
            path,
        )?;
        if words.next() != Some("kB") || words.next().is_some() {
            return Err(invalid(path, "expected memory quantity in kB"));
        }
        *destination =
            Some(MemoryBytes(kib.0.checked_mul(1024).ok_or_else(|| {
                invalid(path, "memory quantity overflows bytes")
            })?));
    }
    let total = total.ok_or_else(|| invalid(path, "missing MemTotal"))?;
    let available = available.ok_or_else(|| invalid(path, "missing MemAvailable"))?;
    if total.0 == 0 || available > total {
        return Err(invalid(path, "inconsistent host memory quantities"));
    }
    Ok(MemorySample { available, total })
}

fn membership(input: &str) -> Result<Option<PathBuf>, MemoryPressureError> {
    let path = Path::new("/proc/self/cgroup");
    let mut unified = None;
    for line in input.lines() {
        let mut fields = line.splitn(3, ':');
        let hierarchy = fields.next();
        let controllers = fields
            .next()
            .ok_or_else(|| invalid(path, "missing cgroup controllers"))?;
        let member = fields
            .next()
            .ok_or_else(|| invalid(path, "missing cgroup membership"))?;
        if controllers
            .split(',')
            .any(|controller| controller == "memory")
        {
            return Err(MemoryPressureError::Unsupported(
                "cgroup v1 memory controller",
            ));
        }
        if hierarchy == Some("0") && controllers.is_empty() {
            if unified.is_some() {
                return Err(invalid(path, "duplicate unified cgroup membership"));
            }
            unified = Some(absolute_path(member, path)?);
        }
    }
    Ok(unified)
}

fn absolute_path(value: &str, source: &Path) -> Result<PathBuf, MemoryPressureError> {
    let path = PathBuf::from(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(invalid(
            source,
            "cgroup path is not an absolute path within its hierarchy",
        ));
    }
    Ok(path)
}

fn unescape_path(value: &str) -> Result<PathBuf, MemoryPressureError> {
    let source = Path::new("/proc/self/mountinfo");
    let mut decoded = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }
        let escape: String = chars.by_ref().take(3).collect();
        decoded.push(match escape.as_str() {
            "040" => ' ',
            "011" => '\t',
            "012" => '\n',
            "134" => '\\',
            _ => return Err(invalid(source, "invalid mount path escape")),
        });
    }
    absolute_path(&decoded, source)
}

fn locate_cgroup(member: &Path, input: &str) -> Result<(PathBuf, PathBuf), MemoryPressureError> {
    let source = Path::new("/proc/self/mountinfo");
    let mut selected: Option<(usize, PathBuf, PathBuf)> = None;
    for line in input.lines() {
        let Some((before, after)) = line.split_once(" - ") else {
            return Err(invalid(source, "missing mount separator"));
        };
        if after.split_whitespace().next() != Some("cgroup2") {
            continue;
        }
        let fields: Vec<_> = before.split_whitespace().collect();
        if fields.len() < 6 {
            return Err(invalid(source, "incomplete cgroup mount"));
        }
        let root = unescape_path(fields[3])?;
        let mount = unescape_path(fields[4])?;
        let Ok(relative) = member.strip_prefix(&root) else {
            continue;
        };
        let directory = mount.join(relative);
        // Prefer the widest visible hierarchy so ancestor limits remain visible.
        let root_depth = root.components().count();
        if selected
            .as_ref()
            .is_none_or(|(previous_depth, _, _)| root_depth < *previous_depth)
        {
            selected = Some((root_depth, mount, directory));
        }
    }
    selected
        .map(|(_, mount, directory)| (mount, directory))
        .ok_or_else(|| invalid(source, "no visible cgroup v2 mount contains this process"))
}

#[cfg(test)]
mod tests;
