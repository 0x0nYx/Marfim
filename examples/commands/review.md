---
description: Review the staged git changes with a security lens
---
Review the staged changes (`git diff --cached`). Focus on:
- Bugs and logic errors
- Security issues (injection, secrets, unsafe deserialization)
- Error handling gaps
- Test coverage for the changed behavior

Report findings as a numbered list with file:line references and severity.
