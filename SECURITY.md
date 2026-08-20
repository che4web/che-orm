# Security Policy

## Supported Versions

Security fixes are applied to the latest version on the `main` branch before a
release is published. This project is experimental and has not yet published a
stable compatibility line.

## Reporting a Vulnerability

Do not open a public issue for a suspected vulnerability. Report it privately
to the repository owner through the contact details in the GitHub profile at
https://github.com/che4web.

Include a description of the impact, affected versions or commits, steps to
reproduce the problem, and a proof of concept when available. You will receive
an acknowledgement and a coordination update as soon as practical.

## Security Boundaries

`che-orm` parameterizes SQL values. Raw SQL expressions accepted by model
attributes such as `default` and `check` are trusted application code and must
not contain untrusted input. Database identifiers are model-controlled; do not
add APIs that interpolate user-provided identifiers into SQL.
