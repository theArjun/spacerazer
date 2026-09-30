# Security policy

SpaceRazer deletes, moves and replaces files, so bugs that could remove data
the user did not choose are treated as security issues. That includes:

- deleting or modifying anything other than the reviewed, staged items
- following a symbolic link or junction during deletion
- skipping revalidation, or acting on an item that changed after staging
- deleting a protected path
- running an external command built from a file name through a shell

## Reporting

Please report privately through
[GitHub security advisories](https://github.com/theArjun/spacerazer/security/advisories/new)
rather than a public issue. Include the platform, SpaceRazer version, and the
steps or folder layout that reproduce the problem.

You can expect an acknowledgement within a few days. Fixes are released as
soon as they are ready, and the advisory credits the reporter unless you ask
otherwise.

## Supported versions

Only the latest release receives fixes.
