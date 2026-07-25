<!--
The CLI flag table (§13.6).

GENERATED from API_SCHEMA.tsv + API_OVERLAY.tsv by
fmn_conformance::schema — regenerate, never hand-edit.
Reference pin: 6199a00d4c1b1127ebe45cb629c3f22538b10e13

Regenerate:  UPDATE_API_ARTIFACTS=1 cargo test -p fmn-conformance \
                 --test api_schema
-->

# The `fmn` flag surface

The Reference's `manimlib/config.py` declares 34 options. Each is kept, re-specified, or dropped by W9 (fm-c53); this table is the inventory that decision is made against, and it is generated, so the inventory cannot quietly shrink.

| Options | Action | Default | Help |
|---|---|---|---|
| `--autoreload` | store_true | — | Automatically reload Python modules to pick up code changes across different files |
| `--clear-cache` | store_true | — | Erase the cache used for Tex and Text Mobjects |
| `--config_file` | store | — | Path to the custom configuration file |
| `--file_name` | store | — | Name for the movie or image file |
| `--finder` | store_true | — | Show the output file in finder |
| `--fps` | store | — | Frame rate, as an integer |
| `--hd` | store_true | — | Render at a 1080p |
| `--leave_progress_bars` | store_true | — | Leave progress bars displayed in terminal |
| `--log-level` | store | — | Level of messages to Display, can be DEBUG / INFO / WARNING / ERROR / CRITICAL |
| `--pix_fmt` | store | — | Pixel format to use for the output of ffmpeg, defaults to `yuv420p` |
| `--prerun` | store_true | — | Calculate total framecount, to display in a progress bar, by doing an initial run of the scene which skips animations. |
| `--show_animation_progress` | store_true | — | Show progress bar for each animation |
| `--subdivide` | store_true | — | Divide the output animation into individual movie files for each animation |
| `--uhd` | store_true | — | Render at a 4k |
| `--vcodec` | store | — | Video codec to use with ffmpeg |
| `--video_dir` | store | — | Directory to write video |
| `-a,--write_all` | store_true | — | Write all the scenes from a file |
| `-c,--color` | store | — | Background color |
| `-e,--embed` | store | — | Adds a breakpoint at the inputted file dropping into an interactive iPython session at that point of the code. |
| `-f,--full_screen` | store_true | — | Show window in full screen |
| `-i,--gif` | store_true | — | Save the video as gif |
| `-l,--low_quality` | store_true | — | Render at 480p |
| `-m,--medium_quality` | store_true | — | Render at 720p |
| `-n,--start_at_animation_number` | store | — | Start rendering not from the first animation, but from another, specified by its index. If you pass in two comma separated values, e.g. "3,6", it will end the rendering at the second value |
| `-o,--open` | store_true | — | Automatically open the saved file once its done |
| `-p,--presenter_mode` | store_true | — | Scene will stay paused during wait calls until space bar or right arrow is hit, like a slide show |
| `-q,--quiet` | store_true | — | — |
| `-r,--resolution` | store | — | Resolution, passed as "WxH", e.g. "1920x1080" |
| `-s,--skip_animations` | store_true | — | Save the last frame |
| `-t,--transparent` | store_true | — | Render to a movie file with an alpha channel |
| `-v,--version` | store_true | — | Display the version of manimgl |
| `-w,--write_file` | store_true | — | Render the scene as a movie file |
| `file` | store | — | Path to file holding the python code for the scene |
| `scene_names` | store | — | Name of the Scene class you want to see |
