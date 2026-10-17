# scripts

Random scripts I wrote to aid in the development of mslicer... Written in [YSH](https://oils.pub/ysh.html).

Most are YSH (`.ysh`). The exception is `viewport-smoke.sh`, a portable bash script that builds and
launches the app headlessly, screenshots the 3D viewport and reports the model's visible bounding
box plus any panic/wgpu errors. See `.omp/skills/mslicer-viewport/SKILL.md` for the workflow and
`--help` for options.
