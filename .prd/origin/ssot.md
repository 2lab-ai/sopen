# origin — fixed target (sopen back to the machine the owner came from, with no ssh that way)

Status: in-progress (opened 2026-11-24)

This file fixes the target of the origin workstream, which covers owner message 24. It is not
reinterpreted during the loop; a change of scope comes only from a new owner message, quoted here
verbatim.

## Owner message (verbatim)

Message 24 (2026-11-24; the owner sent it twice, the first copy cut off mid-sentence; this is the
whole second copy):

> 그리고 별도로 이거 연구해줘 지금은 서로 연결되고 ssh로 접속할수 있어야 모든 기능을 쓸수 있는데
>
> A(터미널) -> B(작업머신) 일경우 ssh B로 해서 B에서 sopen --target A 명령 하면 B에서 "ssh A"가 되야 작동하는데 이걸 좀 생각해봐
>
> A에서 B로 ssh만 될경우에도 B 쉘에서 sopen --target origin origin:~/A.mp3
>
> 이게 되려면 어떻게 해야할까? ssh를 포함한 extended-ssh를 구현해서 ssh 대신 써야할까?
>
> 이거는 별도로 리서치해줘
>
> 의도: A에서 B로만 접속이 가능할경우. B애세 sopen 타겟을 어떻게 A로 할수 있을지. 보안 문제로 B에서는 A로 접속을 못하는 상태로 하고 싶음.
> 일종의 A에서 B로 뭔가 포트를 열어두고 작업을 해야하지 하는 방법 ㅁ라고 가장 엘레강스한 방법이 뭐가 있을까 그냥
>
> A에서 ssh B해서 B에서 자연스럽게 작업하고 싶음
> 아니면 A에서 sopen allow B  같은 명령을 미리 실행하고 ssh B로 가서 할수 있든가(이러면 spen allow B명령이 B로 ssh 타고 들어가서 거시서 뭔가 설정을 미리 해두는 거지 포트 열고 포트 접속할수 있게 키 발급하고 A에서 오픈한 포트를 B ip에서만 접속할수 있게 열고  등등의 철저한 보안처리)

## Fixed scope

1. **The situation.** The owner sits at A (a terminal) and can ssh from A to B (a work machine).
   B must not be able to connect to A: no ssh from B to A, by the owner's security choice.
2. **The goal.** From B's shell, sopen still reaches A as its target: `sopen --target origin …`
   opens on A's screen (A's sopen, A's display) and `origin:~/A.mp3` names a file on A, which B's
   sopen can use. "origin" is the machine the owner's ssh session came from.
3. **Natural use.** The owner types `ssh B` on A and works on B as usual. A one-time setup command
   run on A beforehand (the owner's example: `sopen allow B`, which may go into B over ssh to
   prepare things) is acceptable; so is sopen's own ssh in place of ssh if it is clearly the most
   elegant way. Research decides among these.
4. **Thorough security.** What B can do through the setup is limited to sopen's requests to A,
   nothing more: no shell on A, no ssh to A, nothing reachable on A's network, a key or token per
   allowed machine, and access only from B (the owner's example: "a port A opened that only B's
   address can reach"). The research states the threat model and what a compromised B could still
   do.
5. **This is research first** (OR-R in `loop.md`). No code before the research and a spec.

Readings the workstream made on 2026-11-24 (reversible; the owner may change them):
- Message 23 (xtools runs as a TUI in the terminal by default, `../xtools/ssot.md`) makes the
  owner's terminal on A the natural place for xtools started on B; how A's terminal, A's GUI and
  A's files are reached from B is one design question.
