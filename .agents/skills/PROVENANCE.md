# Pinned Project-Local Agent Instructions

These project-local agent instructions are pinned for deterministic repository
work. The license notices below apply only to the corresponding vendored skill
material; they do not state or change the license of this repository.

| Skill | Repository | Source path | Exact commit | License |
| --- | --- | --- | --- | --- |
| Improve | <https://github.com/shadcn/improve> | `skills/improve` | `03369ee6d7cafbfcecc4346539b05b3dc0a603bb` | MIT |
| Ponytail | <https://github.com/dietrichgebert/ponytail> | `skills/ponytail` | `2ed6c52c9d7e5e56942508591085fd45dea277d3` | MIT |

## License Notices

<!-- BEGIN shadcn/improve LICENSE -->
MIT License

Copyright (c) 2026 shadcn

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
<!-- END shadcn/improve LICENSE -->

<!-- BEGIN dietrichgebert/ponytail LICENSE -->
MIT License

Copyright (c) 2026 DietrichGebert

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
<!-- END dietrichgebert/ponytail LICENSE -->

## Update Procedure

1. Audit upstream changes from the currently pinned commit.
2. Update the exact commit and checksums in Plan 001 or its successor.
3. Reinstall only the narrow upstream skill path into `.agents/vendor-skills/`.
4. Review the complete diff, including provenance and vendored bytes.
