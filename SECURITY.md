# Security policy

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting for this repository:

https://github.com/CuriosityOS/FnType/security/advisories/new

Do not open a public issue for vulnerabilities involving API-key storage, Accessibility permissions, event injection, or transcript disclosure.

## Secrets

FnType stores the xAI API key in macOS Keychain when available. It never belongs in source code, issue reports, logs, screenshots, or committed configuration. If a key is exposed, revoke it immediately in the xAI Console.
