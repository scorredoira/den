//! End-to-end test: a `den` command from a terminal goes to the app that
//! serves them (the window showing the terminal), with its workspace, and
//! the app's answer comes back to it. Its own file, so it runs in its own
//! process (the socket is chosen with an environment variable).

use std::{path::Path, sync::mpsc, time::Duration};

use client::Client;
use proto::{Event, Request, Response};

#[test]
fn the_app_answers_commands_from_its_terminals() {
    let dir = std::env::temp_dir().join(format!("den-agent-commands-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    // SAFETY: the test is the only thread touching the environment.
    unsafe {
        std::env::set_var("DEN_AGENT_SOCKET", dir.join("agent.sock"));
        std::env::set_var("DEN_STATE_DIR", dir.join("state"));
        std::env::set_var("DEN_CONFIG_DIR", dir.join("config"));
    }
    let agent = Path::new(env!("CARGO_BIN_EXE_den-agent"));
    let app = Client::connect_local(agent).unwrap();
    let cli = Client::connect_local(agent).unwrap();
    let request = |client: &Client, request| smol::block_on(client.request(request));

    // no app serves commands yet
    let command = |term| Request::Command { args: vec!["tabs".into()], cwd: dir.clone(), term };
    let err = request(&cli, command(None)).unwrap_err();
    assert!(err.to_string().contains("no den app"), "{err:#}");

    // a terminal of the workspace `dir` knows its own id
    let group = dir.to_string_lossy().into_owned();
    let Ok(Response::TermCreated { term }) = request(
        &app,
        Request::TermCreate {
            group: group.clone(),
            cwd: dir.clone(),
            command: Some(if cfg!(windows) {
                vec!["cmd.exe".into(), "/C".into(), "echo term=%DEN_TERM% & ping -n 30 127.0.0.1 >nul".into()]
            } else {
                vec!["/bin/sh".into(), "-c".into(), "echo term=$DEN_TERM; sleep 30".into()]
            }),
            cols: 80,
            rows: 24,
        },
    ) else {
        panic!("no terminal");
    };
    let expected = format!("term={term}");
    let mut screen = String::new();
    for _ in 0..100 {
        if let Ok(Response::Text(text)) = request(&cli, Request::TermRead { term, lines: 10 }) {
            screen = text;
        }
        if screen.contains(&expected) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(screen.contains(&expected), "{screen}");

    // the app gets the command with the terminal's workspace and answers it
    let (tx, commands) = mpsc::channel();
    app.watch(move |event| {
        if let Event::Command { command, args, term, group, .. } = event {
            let _ = tx.send((*command, args.clone(), *term, group.clone()));
        }
    });
    request(&app, Request::Serve).unwrap();
    let (answer_tx, answer) = mpsc::channel();
    cli.request_with(command(Some(term)), move |result| {
        let _ = answer_tx.send(result);
    });
    let (id, args, from, from_group) = commands.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!((args, from, from_group), (vec!["tabs".to_string()], Some(term), Some(group)));
    app.notify(Request::CommandDone { command: id, result: Ok("main.rs".into()) });
    let Ok(Response::Text(text)) = answer.recv_timeout(Duration::from_secs(5)).unwrap() else {
        panic!("no answer");
    };
    assert_eq!(text, "main.rs");

    // and its errors; outside den's terminals there's no workspace
    let pending = cli.request(command(None));
    let (id, _, from, from_group) = commands.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!((from, from_group), (None, None));
    app.notify(Request::CommandDone { command: id, result: Err("no file is open".into()) });
    let err = smol::block_on(pending).unwrap_err();
    assert!(err.to_string().contains("no file is open"), "{err:#}");

    // another window of the app, serving last: commands from the terminal
    // still go to the window showing it, the others to the last
    let other = Client::connect_local(agent).unwrap();
    let (other_tx, other_commands) = mpsc::channel();
    other.watch(move |event| {
        if let Event::Command { command, .. } = event {
            let _ = other_tx.send(*command);
        }
    });
    request(&other, Request::Serve).unwrap();
    request(&app, Request::TermAttach { term }).unwrap();
    let pending = cli.request(command(Some(term)));
    let (id, ..) = commands.recv_timeout(Duration::from_secs(5)).unwrap();
    app.notify(Request::CommandDone { command: id, result: Ok(String::new()) });
    smol::block_on(pending).unwrap();
    let pending = cli.request(command(None));
    let id = other_commands.recv_timeout(Duration::from_secs(5)).unwrap();
    other.notify(Request::CommandDone { command: id, result: Ok(String::new()) });
    smol::block_on(pending).unwrap();
    assert!(commands.try_recv().is_err());

    request(&app, Request::Shutdown).ok();
}
