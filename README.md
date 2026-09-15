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
cargo test --workspace     # 62 tests
cargo run -p remote-assist # opens the Host and Helper windows
```

Click **Generate Code** in the Host window, type it into the Helper
window, press **Confirm** on both sides. Once active, move/click inside
the Helper window's control box to send real input to the Host. Press
`Ctrl+Alt+End` anywhere (or **End Session**) to end it instantly, in
every mode.
