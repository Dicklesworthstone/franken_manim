<!--
The CLI flag table (§13.6).

GENERATED from API_SCHEMA.tsv + API_OVERLAY.tsv by
fmn_conformance::schema — regenerate, never hand-edit.
Reference pin: 6199a00d4c1b1127ebe45cb629c3f22538b10e13

Regenerate:  UPDATE_API_ARTIFACTS=1 cargo test -p fmn-conformance \
                 --test api_schema
-->

# The `fmn` flag surface

The Reference's `manimlib/config.py` declares 34 options. Every row has exactly one authored ruling and generated-parser binding; coverage is fail-closed, so the inventory cannot quietly shrink.

## Reference flags

| Options | Command | Binding | Status | Action | Default | Semantics |
|---|---|---|---|---|---|---|
| `--autoreload` | render | `autoreload` | tiered | store_true | false | re-specified as supervised Studio worker reload |
| `--clear-cache` | global | `clear_cache` | improved | store_true | false | safe content-store lifecycle operation |
| `--config_file` | global | `config_file` | same | store | — | defaults then user file then CLI precedence |
| `--file_name` | render | `file_name` | same | store | — | explicit output stem |
| `--finder` | render | `finder` | improved | store_true | false | portable reveal-in-file-manager action |
| `--fps` | render | `fps` | same | store | — | positive integer frame rate |
| `--hd` | render | `hd` | same | store_true | false | 1080p resolution preset |
| `--leave_progress_bars` | render | `leave_progress_bars` | same | store_true | false | human progress policy |
| `--log-level` | global | `log_level` | same | store | — | stable log-level vocabulary |
| `--pix_fmt` | render | `pix_fmt` | same | store | — | explicit ffmpeg wire pixel format |
| `--prerun` | render | `prerun` | improved | store_true | false | deterministic count-only scene pass |
| `--show_animation_progress` | render | `show_animation_progress` | same | store_true | false | per-animation human progress |
| `--subdivide` | render | `subdivide` | same | store_true | false | one output per animation segment |
| `--uhd` | render | `uhd` | same | store_true | false | 2160p resolution preset |
| `--vcodec` | render | `vcodec` | same | store | — | explicit ffmpeg encoder |
| `--video_dir` | render | `video_dir` | same | store | — | explicit output directory |
| `-a,--write_all` | render | `write_all` | same | store_true | false | select every scene in declaration order |
| `-c,--color` | render | `background` | same | store | — | explicit background color |
| `-e,--embed` | render | `embed` | tiered | store | — | Python front door or Studio breakpoint only |
| `-f,--full_screen` | render | `full_screen` | same | store_true | false | fullscreen interactive presentation |
| `-i,--gif` | render | `gif` | same | store_true | false | native GIF output |
| `-l,--low_quality` | render | `low_quality` | same | store_true | false | 480p resolution preset |
| `-m,--medium_quality` | render | `medium_quality` | same | store_true | false | 720p resolution preset |
| `-n,--start_at_animation_number` | render | `animation_range` | improved | store | — | validated start or half-open start,end play range |
| `-o,--open` | render | `open` | improved | store_true | false | portable host open action |
| `-p,--presenter_mode` | render | `presenter_mode` | same | store_true | false | presenter-controlled waits |
| `-q,--quiet` | global | `quiet` | same | store_true | false | suppress human decoration only |
| `-r,--resolution` | render | `resolution` | improved | store | — | validated WIDTHxHEIGHT override |
| `-s,--skip_animations` | render | `skip_animations` | same | store_true | false | capture the final state |
| `-t,--transparent` | render | `transparent` | improved | store_true | false | alpha-capable output negotiation |
| `-v,--version` | global | `version` | same | store_true | false | stable version report |
| `-w,--write_file` | render | `write_file` | same | store_true | false | durable output instead of preview-only |
| `file` | render | `file` | same | store | — | scene source path |
| `scene_names` | render | `scene_names` | same | store | — | explicit scene-name selection |

## Native flags

| Options | Command | Binding | Status | Action | Default | Help |
|---|---|---|---|---|---|---|
| `-h,--help` | global | `help` | improved | store_true | False | Show command help and exit |
| `--robot` | global | `robot` | improved | store_true | False | Emit only schema-versioned NDJSON records |
| `--reproducible` | global | `reproducible` | improved | store_true | False | Select certified CPU rendering and canonical certified artifacts |
| `--threads` | global | `threads` | improved | store | — | Bound render-team CPU threads |
| `--ffmpeg` | global | `ffmpeg` | improved | store | — | Absolute path to the optional ffmpeg executable |
| `--cache-dir` | global | `cache_dir` | improved | store | — | Explicit FrankenManim content-store root |
| `--format` | render | `format` | improved | store | auto | Select auto, png, png_sequence, gif, y4m, wav, or video output |
| `--math-pack` | render | `math_pack` | improved | store | default | Select an fmd-math preamble pack |
| `--budget-ms` | batch | `budget_ms` | improved | store | — | Wall-clock budget for the batch |
| `--max-scenes` | batch | `max_scenes` | improved | store | — | Bound simultaneously active scene jobs |
| `--fail-fast` | batch | `fail_fast` | improved | store_true | False | Cancel remaining jobs after the first failure |
| `--manifest-dir` | batch | `manifest_dir` | improved | store | — | Write per-scene manifests under this directory |
| `--require-ffmpeg` | doctor | `require_ffmpeg` | improved | store_true | False | Exit with capability status when ffmpeg is unavailable |
| `--bind` | studio | `bind` | improved | store | 127.0.0.1 | Loopback address for the Studio host |
| `--port` | studio | `port` | improved | store | 0 | Studio TCP port, with zero selecting an ephemeral port |
| `--no-browser` | studio | `no_browser` | improved | store_true | False | Do not launch the host browser |
| `--tui` | studio | `tui` | improved | store_true | False | Attach the kitty or sixel terminal client |
| `--checkpoint-frames` | studio | `checkpoint_frames` | improved | store | 120 | Maximum frames between replay checkpoints |
| `--preview-codec` | studio | `preview_codec` | improved | store | png | Select permanent multipart PNG or optional MJPEG preview |

## Commands

| Command | Status | Meaning |
|---|---|---|
| `render` | same | Render one or more selected scenes |
| `doctor` | improved | Report capabilities and the derived ExecutionPlan |
| `batch` | improved | Render a bounded multi-scene farm under asupersync |
| `studio` | improved | Run the isolated-worker live Studio |

## Exit codes

| Code | Identity | Meaning |
|---:|---|---|
| 0 | `success` | The requested command completed |
| 2 | `usage` | Arguments, flag interactions, or selection syntax are invalid |
| 3 | `config` | A configuration file or resolved value is invalid |
| 4 | `capability` | A required optional capability is unavailable |
| 5 | `scene` | Scene discovery, construction, or execution failed |
| 6 | `render` | Rasterization, output, or publication failed |
| 7 | `cancelled` | The operation was cooperatively cancelled |
| 8 | `budget` | A declared time, memory, or concurrency budget was exhausted |
| 70 | `internal` | An invariant failed inside FrankenManim |

## Flag interactions

These rules are emitted into the parser artifact and executed after token collection. `implies` rules are non-failing; every other rule names the stable exit identity returned on violation.

| Rule | Kind | Bindings | Exit | Diagnostic |
|---|---|---|---|---|
| `quality-exclusive` | at_most_one | `low_quality`, `medium_quality`, `hd`, `uhd`, `resolution` | usage | choose at most one quality preset or explicit resolution |
| `open-writes` | implies | `open`, `write_file` | — | --open implies durable output |
| `finder-writes` | implies | `finder`, `write_file` | — | --finder implies durable output |
| `gif-writes` | implies | `gif`, `write_file` | — | --gif implies durable output |
| `transparent-writes` | implies | `transparent`, `write_file` | — | --transparent implies durable output |
| `format-writes` | implies | `format`, `write_file` | — | --format implies durable output |
| `certified-writes` | implies | `reproducible`, `write_file` | — | --reproducible implies durable canonical output |
| `codec-writes` | implies | `vcodec`, `write_file` | — | --vcodec implies durable output |
| `pixel-format-writes` | implies | `pix_fmt`, `write_file` | — | --pix_fmt implies durable output |
| `subdivide-writes` | implies | `subdivide`, `write_file` | — | --subdivide implies durable output |
| `filename-writes` | implies | `file_name`, `write_file` | — | --file_name implies durable output |
| `video-directory-writes` | implies | `video_dir`, `write_file` | — | --video_dir implies durable output |
| `gif-vs-non-gif-format` | conflicts | `gif`, `format=auto,png,png_sequence,y4m,wav,video` | usage | --gif conflicts with a non-GIF --format |
| `native-format-vs-codec` | conflicts | `format=png,png_sequence,gif,y4m,wav`, `vcodec` | usage | --vcodec applies only to ffmpeg video output |
| `native-format-vs-pixel-format` | conflicts | `format=png,png_sequence,gif,y4m,wav`, `pix_fmt` | usage | --pix_fmt applies only to ffmpeg video output |
| `transparent-vs-opaque-pixel-format` | conflicts | `transparent`, `pix_fmt=yuv420p,yuv422p,yuv444p,nv12,p010,p010le,rgb24,bgr24` | usage | --transparent requires an alpha-capable output pixel format |
| `transparent-vs-opaque-native-format` | conflicts | `transparent`, `format=y4m,wav` | usage | --transparent is incompatible with y4m and WAV outputs |
| `skip-vs-subdivide` | conflicts | `skip_animations`, `subdivide` | usage | --skip_animations cannot produce per-animation subdivisions |
| `write-all-vs-selection` | conflicts | `write_all`, `scene_names` | usage | --write_all cannot accompany explicit scene names |
| `embed-vs-certified` | conflicts | `embed`, `reproducible` | usage | interactive embed points are outside certified execution |
| `reload-vs-certified` | conflicts | `autoreload`, `reproducible` | usage | a changing source tree is outside one certified input closure |
| `clear-cache-only` | exclusive | `clear_cache`, `robot`, `quiet`, `config_file`, `cache_dir`, `log_level` | usage | --clear-cache is a standalone lifecycle action |
| `version-only` | exclusive | `version`, `robot` | usage | --version is a standalone query |
| `help-only` | exclusive | `help`, `robot` | usage | --help is a standalone query for the selected command |
