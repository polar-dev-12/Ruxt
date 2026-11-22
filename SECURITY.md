# Security Policy / 보안 정책

Ruxt is a security-first SSH/SFTP client. If you find a way to make it leak credentials,
session data or local files, or to bypass host-key pinning, tunnel/X11 isolation or the
credential vault, please tell us privately first.

## Reporting a vulnerability

* **Preferred:** GitHub private vulnerability reporting on this repository
  (*Security → Report a vulnerability*). It reaches the maintainers only.
* If that is not available, open an issue titled "security contact request" **without
  details** and a maintainer will reply with a private channel.
* Please include: the version (`ruxt --version` or *About*), platform, reproduction steps,
  and what an attacker gains. A proof of concept is welcome; exploit code is not needed.

We aim to acknowledge reports within **48 hours** and to publish a fix or mitigation within
**14 days** for high-severity issues. You will be credited in the changelog unless you
prefer otherwise.

## Scope

In scope: everything under `src/`, the installer scripts under `installer/`, the release
workflow, and the bundled Cygwin/X build configuration.

Out of scope (see `docs/THREAT_MODEL.md`): malware running under the same user account,
kernel/hardware attacks, vulnerabilities in the upstream X server or SSH servers you connect
to, and anything requiring the user to disable a documented security default.

## Supported versions

Only the latest release receives security fixes.

---

## 취약점 신고 (한국어)

* 공개 이슈 대신 저장소의 **비공개 취약점 신고**(Security → Report a vulnerability)를 써 주세요.
* 버전, 플랫폼, 재현 절차, 공격자가 얻는 것을 적어 주시면 **48시간 안에** 확인 답변을,
  심각한 문제는 **14일 안에** 수정 또는 완화책을 목표로 합니다.
* 범위와 범위 밖은 위의 영어 설명과 `docs/THREAT_MODEL.md`를 따릅니다. 최신 릴리스만 보안 수정을 받습니다.
