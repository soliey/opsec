```
  .-')                                     ('-.              
 ( OO ).                                 _(  OO)             
(_)---\_) .-'),-----.  ,--.      ,-.-') (,------. ,--.   ,--.
/    _ | ( OO'  .-.  ' |  |.-')  |  |OO) |  .---'  \  `.'  / 
\  :` `. /   |  | |  | |  | OO ) |  |  \ |  |    .-')     /  
 '..`''.)\_) |  |\|  | |  |`-' | |  |(_/(|  '--.(OO  \   /   
.-._)   \  \ |  | |  |(|  '---.',|  |_.' |  .--' |   /  /\_  
\       /   `'  '-'  ' |      |(_|  |    |  `---.`-./  /.__) 
 `-----'      `-----'  `------'  `--'    `------'  `--'      
```


## Run it
download the latest [installer](github.com/soliey/opsec/releases)

download the *source code*
Requires the Rust MSVC toolchain (Visual Studio Build Tools, "Desktop
development with C++" workload) on Windows.

```
cargo test --workspace     # 114 tests
cargo run -p remote-assist # opens the Host and Helper windows
```

Click **Generate Code** in the Host window, type it into the Helper
window, press **Confirm** on both sides. Once active, move/click inside
the Helper window's control box to send real input to the Host, and
watch the Host's real screen stream into the Helper window (H.264,
encrypted, adaptive to your chosen bandwidth profile, near-idle while the
screen isn't changing). Press `Ctrl+Alt+End` anywhere (or **End
Session**) to end it instantly, in every mode.
