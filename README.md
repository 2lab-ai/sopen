# sopen

Open a file or folder on another machine as if it were local:

```sh
sopen myserver:/var/log/app.log    # text → Zed remote
sopen myserver:~/project           # folder → Zed remote, as a project
sopen myserver:/tmp/shot.png       # image → view, rectangle crop / pixelize, save back
sopen myserver:/tmp/talk.mp4       # video → play, rectangle crop, time cut → new file next to it
sopen myserver:/tmp/a.mp3          # audio → play, time cut
```

Same syntax as `scp`. Uses your `ssh` and `~/.ssh/config`; the remote needs nothing but sshd.

Status: under construction — see [`.prd/01-spec.md`](.prd/01-spec.md).

License: MIT
