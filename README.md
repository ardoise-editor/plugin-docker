# Docker — an Ardoise Editor plugin

Docker and Docker Compose in [Ardoise Editor](https://github.com/ardoise-editor/ardoise), after the Docker tool window of the JetBrains IDEs. It drives the `docker` command: what your shell sees (contexts, `DOCKER_HOST`, remote engines) is what the plugin sees.

## The panel

A tree of the **Compose** projects (project → service → container), the other **containers**, the **images**, **volumes** and **networks**, with a filter and the choice of the Docker context. It follows `docker events`. A right click on a row gives its actions.

- **Containers**: new (`run` or `create`: ports, volumes, variables, network, restart policy…), start, stop, restart, pause, kill, remove, logs, shell (as the user or as root), a command of your own, attach, save as an image.
- **Images**: pull, push, tag, run, layers, save to and load from an archive, remove.
- **Volumes and networks**: create, inspect, remove; browse what a volume holds (below); connect a container to a network or disconnect it.
- **Registries**: see below.
- **Compose**: up, up with a build, recreate, start, stop, restart, logs, pull, build, scale, down (with or without the volumes and the built images).

A click on a container opens its tab: information (ports, networks, mounts, variables), **files**, processes, statistics and the whole `inspect`.

## The files of a container

Browse its folders; open a file in the editor (saving it writes it back, keeping its owner and rights); make files and folders, rename, delete (several rows at once), copy from this machine and download — as the user of the image or as root. Listing and changing files needs a running container with `sh` (not a distroless image); downloading and copying in work on a stopped one too.

## What a volume holds

A click on a volume opens its files, with what the files of a container offer (browse, edit, create, rename, delete, copy in and out). The plugin looks into it through a container of its own (`busybox:stable`, pulled if needed): no network, hidden from the lists, removed when the tab is closed or the volume removed, and when the editor quits. If the editor is killed, it ends by itself within an hour, or at the next start of the plugin. It runs as root: new files belong to root. Changing the files of a volume a running service uses (a database) can corrupt its data.

## Registries

Docker Hub, the registries `docker` is logged in to and those you add. **Log in** runs `docker login` (the password only on its standard input; `docker` keeps it as it usually does) and **Log out** `docker logout`: the command line of the machine shares both. A tab per registry lists its repositories (where it allows: Docker Hub and the big forges don't), searches Docker Hub, and shows the tags of a repository, to pull, run or copy. **Push** on an image asks for the registry, the repository and the tag, then tags and pushes.

To list them, the plugin asks the registry's API with what `docker` kept (its configuration or its credential helper): sent only to that registry and to the token server it names, over HTTPS (HTTP for `localhost` only). The certificates of the system are trusted. Identity tokens (some cloud registries) are not used: such a registry is asked without an account.

## Cleanup

Disk usage by kind, then what to remove: stopped containers, dangling or unused images, unused networks, anonymous or all unused volumes, build cache — optionally only what is older than a given age (not for volumes). Each box ticked says what it would remove; the commands are shown and confirmed before anything runs, and the room won back is reported. Only dangling images and unused build cache are ticked at first.

## In Dockerfiles and compose files

`Dockerfile`, `Dockerfile.*`, `*.dockerfile`, `Containerfile`, `compose*.yml` and `docker-compose*.yml`:

- **▶ in the gutter** (Ardoise 2.10+) on `services:`, on each service and on each `FROM`: up, logs, shell… of that service, or build (and run) up to that stage;
- the same buttons at the top of the panel (any version), and the command **Docker: Run This File…**.

A `compose.override.yml` beside a default compose file goes with it.

**Completion** in Dockerfiles (Ardoise 2.10+, as you type and on Ctrl+Space): instructions with what they do, their options and values (`--from=`, `--platform=`, `--mount=`…), after `FROM` the earlier stages, the images of the engine and common official ones, the variables of `ARG` and `ENV` after `$`, and the files of the build context after `COPY` and `ADD`.

## The console

Shells, logs, builds, pulls and Compose run in real terminals, one tab each, under the editor.

## Build

```sh
cargo test       # the tests of tests/drive.rs use the Docker engine of the machine, if any
cargo build --release && mkdir -p bin && cp target/release/docker-plugin bin/
```

Then in Ardoise: **Preferences › Plugins › Link a folder…** and pick this folder.

`tests/drive.rs` only makes things named `ardoise-test-…` (it may pull `busybox:stable` and `registry:2`, a registry of its own on `localhost`), gives the plugin a Docker configuration of its own, and a `docker` that refuses every `prune`.

## License

MIT
