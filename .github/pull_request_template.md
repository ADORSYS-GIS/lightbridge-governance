## Summary

Brief description of what this PR does.

## Related Issues

Closes #

## Type of Change

- [ ] Bug fix
- [ ] New feature
- [ ] Breaking change
- [ ] Documentation update
- [ ] Refactoring
- [ ] CI/CD or tooling

## Changes Made

-
-

## Checklist

- [ ] Self-review completed
- [ ] Tests added/updated for changes
- [ ] All tests pass (`just all-checks`)
- [ ] Documentation updated if needed

## Design Review

State the **mechanism**, not the verdict. "This clones per request" is actionable; "looks fine" is not.

1. **Failure modes** — State the mechanism by which this code fails closed when a dependency is unavailable, and what a missing or unparseable attribute resolves to — "unknown" routes to the strictest branch, it is never a default. Identify any `unwrap_or(false)` on a check; that is an outage becoming an authorization bypass.
2. **Ownership shape** — State whether any new function takes `T` where `&T` would do, forcing callers to clone, and name any clone in a per-request path. `Arc::clone` to satisfy a `'static` bound is a refcount bump, not a defect — distinguish them from value clones.
3. **Error type level** — State which cases a caller can distinguish from any new error variants, and whether those variants stay at this crate's abstraction level. Leaking a dependency's error type into a public enum is a breaking change on every dep bump.
4. **Tests prove the logic** — State what would happen to each new test if the logic it covered were wrong. A test that cannot fail regardless of the implementation adds no coverage.

## Security Considerations

Does this PR have security implications? (authentication, certificates, cryptography, etc.)

- [ ] No security impact
- [ ] Security impact reviewed (describe below)

## Testing

How was this tested?

- [ ] Unit tests
- [ ] Integration tests (`just up && just migrate && just test`)
- [ ] Manual testing
- [ ] Other:

---

## Intent / Source of Truth

Why this change exists, and the source of truth (issue link, ADR, decision, or incident).

Source of truth: <link or #issue>

## Verification

Evidence this change works — commands run, output, screenshots, or links (see **Testing** above).

- [ ] Verified locally (`just all-checks`)

## AI Usage Declaration

Declare whether/how AI was used:

- [ ] Not used
- [ ] AI-assisted (describe what AI was used for below)

> AI may accelerate the work, but it must not launder ignorance into polished artifacts.
> Governance: https://adorsys-gis.github.io/ai-governance/
