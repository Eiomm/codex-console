use super::*;
use model::*;
use serde_json::json;
use std::{collections::BTreeSet, fs::{self, OpenOptions}, io::Write, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}};

const CODEX: &str = include_str!("../fixtures/events/codex_rollout.jsonl");
const CLAUDE: &str = include_str!("../fixtures/events/claude_stream.jsonl");
fn codex_events() -> (Vec<RunEvent>, u64) {
    let mut parser = CodexParser::new();
    let events = CODEX.lines().flat_map(|l| parser.feed_line(l)).collect();
    (events, parser.skipped)
}
fn claude_events() -> (Vec<RunEvent>, u64) {
    let mut parser = claude::ClaudeParser::new();
    let events = CLAUDE.lines().flat_map(|l| parser.feed_line(l)).collect();
    (events, parser.skipped)
}
fn token_events(events: &[RunEvent]) -> Vec<&Tokens> {
    events.iter().filter_map(|e| if let EventKind::Tokens { tokens } = &e.kind { Some(tokens) } else { None }).collect()
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!("event-tests-{}-{}-{}", std::process::id(), time::now_ms(), ID.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&p).unwrap(); Self(p)
    }
}
impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn append(path: &Path, bytes: &[u8]) { OpenOptions::new().append(true).open(path).unwrap().write_all(bytes).unwrap(); }
fn fixture_session(root: &Path, day: i64, id: &str, cwd: &str, ts: &str) -> PathBuf {
    let dir = root.join(time::date_path(day)); fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-{id}.jsonl"));
    fs::write(&path, format!("{}\n", json!({"type":"session_meta","timestamp":ts,"payload":{"id":id,"cwd":cwd,"originator":"test"}}))).unwrap(); path
}
#[test]
fn codex_fixture_exact_acceptance() {
    let (events, skipped) = codex_events();
    let tokens = token_events(&events);
    let last = &tokens.last().unwrap().usage;
    assert_eq!((last.input,last.cached_input,last.output,last.reasoning,last.total,last.fresh), (132233,122368,1116,11,133349,10981));
    assert_eq!(tokens.iter().map(|t| t.delta).sum::<u64>(), 133349);
    assert!(tokens.iter().all(|t| !t.provisional));
    assert_eq!(events.iter().filter(|e| matches!(e.kind, EventKind::Command { .. })).count(), 8);
    let files: BTreeSet<_> = events.iter().filter_map(|e| if let EventKind::FileChange { item_id, path, .. } = &e.kind { assert_eq!(path,"slug.js"); Some(item_id) } else { None }).collect();
    assert_eq!(files.len(), 2);
    assert_eq!(events.iter().filter(|e| matches!(&e.kind, EventKind::Message { role, .. } if role == "assistant")).count(),4);
    assert!(events.iter().any(|e| matches!(&e.kind,EventKind::Session { session } if session.service_tier.as_deref()==Some("priority") && session.model.as_deref()==Some("gpt-6-astra"))));
    assert!(events.iter().any(|e| matches!(e.kind,EventKind::TurnCompleted { .. })));
    assert!(skipped >= 2);
    let serialized = serde_json::to_value(tokens.last().unwrap()).unwrap();
    assert_eq!(serialized["cachedInput"],122368);
}
#[test]
fn claude_fixture_exact_acceptance_and_final_replacement() {
    let (events, skipped) = claude_events();
    let modes: Vec<_> = events.iter().filter_map(|e| if let EventKind::Handoff { mode, .. } = e.kind {Some(mode)} else {None}).collect();
    assert_eq!(modes, [HandoffMode::Fresh,HandoffMode::Resume]);
    let tokens = token_events(&events);
    assert_eq!(tokens.len(),9);
    assert_eq!(tokens.iter().filter(|t|t.scope==Scope::Main).count(),7);
    assert_eq!(tokens.iter().filter(|t|t.scope==Scope::Sub).count(),2);
    assert!(tokens.iter().all(|t|t.provisional));
    let mut main = Usage::default(); for t in tokens.iter().filter(|t|t.scope==Scope::Main) {main.add(&t.usage);}
    assert_eq!((main.input,main.cache_write,main.cached_input,main.output), (14,51941,272379,170));
    assert_eq!(main.fresh,14+51941+170);
    let result = events.iter().find(|e|matches!(e.kind,EventKind::RunResult{..})).unwrap();
    if let EventKind::RunResult { cost_usd,num_turns,per_model,.. } = &result.kind {
        assert!((cost_usd-0.6580444).abs()<1e-10); assert_eq!(*num_turns,11);
        assert_eq!(per_model["claude-opus-5-5"].output,6236); assert_eq!(per_model["claude-sonnet-5-5"].output,1900);
    }
    let mut summary = RunSummary::default(); for event in &events {summary.apply(event);}
    let claude = &summary.agents[&Agent::Claude];
    assert_eq!(claude.usage.output,8136); assert!(!claude.provisional);
    assert_eq!(claude.usage.input,22); assert_eq!(claude.usage.cache_write,51941+16982);
    // A duplicate final record and late provisional usage cannot add to authoritative totals.
    summary.apply(result); summary.apply(events.iter().find(|e| matches!(e.kind,EventKind::Tokens{..})).unwrap());
    assert_eq!(summary.agents[&Agent::Claude].usage.output,8136);
    assert!(!summary.agents[&Agent::Claude].provisional);
    assert!(skipped>0);
}
#[test]
fn summary_mixed_arrivals_and_chronological_arrivals() {
    let mut events = claude_events().0; events.extend(codex_events().0);
    let mut a=RunSummary::default(); for event in &events {a.apply(event);}
    events.sort_by_key(|e|e.ts_ms);
    let mut b=RunSummary::default(); for event in &events {b.apply(event);}
    for summary in [&a,&b] {
        assert_eq!(summary.round,2); assert!(summary.fast);
        assert_eq!(summary.agents[&Agent::Codex].usage.total,133349);
        assert_eq!(summary.agents[&Agent::Codex].commands,8);
        assert_eq!(summary.agents[&Agent::Claude].usage.output,8136);
        assert!(!summary.agents[&Agent::Claude].provisional);
        assert_eq!(summary.phase,summary::Phase::Gate);
        assert_eq!(summary.files.len(),1);
        assert!(summary.phases[&summary::Phase::Review].tools>0);
    }
    assert_eq!(serde_json::to_value(a).unwrap(),serde_json::to_value(b).unwrap());
}
#[test]
fn fixtures_are_safe_and_serialization_is_flat_camel_case() {
    let mut events=codex_events().0; events.extend(claude_events().0);
    let text=serde_json::to_string(&events).unwrap();
    for forbidden in ["creator_","encrypted","signature","base_instructions"] {assert!(!text.contains(forbidden),"{forbidden}");}
    let value=serde_json::to_value(&events[0]).unwrap();
    assert_eq!(value["kind"],"session"); assert!(value.get("tsMs").is_some()); assert!(value.get("sourceId").is_some());
}
#[test]
fn redact_keys_bearer_unicode_email_and_homes() {
    let mut parser=CodexParser::with_home(Some("/custom/home".into()));
    let line=json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","command":["sh","-c","echo /custom/home/a"],"aggregated_output":"sk-abcdefghijklmnopqrstuvwxyz123456 Bearer abc.def ghp_abcdefghijklmnopqrstuvwxyz xoxb-abcdefghijklmnopqrstuvwxyz /Users/alice/repo /home/bob/x alice@example.com"}}});
    let event=parser.feed_line(&line.to_string()).pop().unwrap();
    let value=serde_json::to_value(event).unwrap(); let output=value["outputTail"].as_str().unwrap();
    for secret in ["sk-","abc.def","ghp_","xoxb-","alice","bob"] {assert!(!output.contains(secret));}
    assert!(output.contains("[已隐藏]")); assert!(output.contains("~/repo")); assert_eq!(value["command"],"echo ~/a");
    let r=redact::Redactor::default(); let long="中文🙂".repeat(100000);
    let (head,truncated)=r.text(&long); assert_eq!(head.chars().count(),4000); assert!(truncated);
    let (tail,truncated)=r.tail(&long); assert_eq!(tail.chars().count(),4000); assert!(truncated);
    let (diff,truncated)=r.diff(&long); assert!(diff.len()<=65536); assert!(truncated);
    assert_eq!(r.clean("sk-short"),"sk-short");
    assert_eq!(r.clean("creator_private-value"),"creator_private-value");
}
#[test]
fn codex_dedup_context_file_stats_reasoning_and_unknowns() {
    let mut p=CodexParser::new();
    let context=json!({"type":"turn_context","payload":{"cwd":"/repo","model":"m","sandbox_policy":{"type":"read-only"}}}).to_string();
    assert_eq!(p.feed_line(&context).len(),1); assert!(p.feed_line(&context).is_empty());
    for kind in ["token_usage_record","event_msg"] {
        let usage=json!({"total_tokens":20,"input_tokens":15,"cached_input_tokens":10,"output_tokens":5});
        let line=if kind=="event_msg" {json!({"type":kind,"payload":{"type":"token_count","info":{"total_token_usage":usage}}})} else {json!({"type":kind,"payload":{"thread_token_usage":usage}})};
        assert_eq!(p.feed_line(&line.to_string()).len(),usize::from(kind=="token_usage_record"));
    }
    let lines=[json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"Reasoning","summary_text":[]}}}),json!({"type":"response_item","payload":{"encrypted_content":"private"}}),json!({"type":"world_state"})];
    for line in lines {assert!(p.feed_line(&line.to_string()).is_empty());}
    assert!(p.skipped>=2);
    let line=json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","id":"f","changes":{"/repo/a":{"type":"update","unified_diff":"--- a\n+++ b\n+你好\n-旧\n context\n","move_path":"/repo/b"},"/repo/c":{"type":"add","unified_diff":"+new"}}}}});
    let events=p.feed_line(&line.to_string());assert_eq!(events.len(),2);
    if let EventKind::FileChange {added,removed,move_path,..}=&events[0].kind {assert_eq!((*added,*removed),(1,1));assert_eq!(move_path.as_deref(),Some("b"));} else {panic!();}
}
#[test]
fn claude_blocks_tool_correlation_and_skipped() {
    let mut p=claude::ClaudeParser::new();
    let events=p.feed_line(&json!({"type":"assistant","session_id":"s","message":{"id":"m","usage":{"input_tokens":2,"output_tokens":3},"content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"pwd","description":"ignored"}},{"type":"text","text":"hi"}]}}).to_string());
    assert_eq!(events.len(),3);
    assert!(matches!(&events[0].kind,EventKind::Tool{tool_use_id,summary,..} if tool_use_id=="t" && summary=="pwd"));
    let result=p.feed_line(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":"bad"}],"is_error":true}]}}).to_string());
    assert!(matches!(&result[0].kind,EventKind::ToolResult{tool_use_id,text,is_error:true,..} if tool_use_id=="t" && text=="bad"));
    for line in ["null","{broken",r#"{"type":"future"}"#] {assert!(p.feed_line(line).is_empty());} assert_eq!(p.skipped,3);
}
#[test]
fn tail_partial_utf8_truncate_and_replace() {
    let tmp=Temp::new();let path=tmp.0.join("tail");let mut tail=Tailer::new();
    fs::write(&path,b"one\npar").unwrap(); assert_eq!(tail.poll(&path).unwrap().lines,["one"]);
    append(&path,b"tial\n\xe4\xb8");assert_eq!(tail.poll(&path).unwrap().lines,["partial"]);
    append(&path,b"\xad\n\xff\n");assert_eq!(tail.poll(&path).unwrap().lines,["中","�"]);
    fs::write(&path,b"new\n").unwrap();let batch=tail.poll(&path).unwrap();assert!(batch.reset);assert_eq!(batch.lines,["new"]);
    let replacement=tmp.0.join("replace");fs::write(&replacement,b"replaced\n").unwrap();fs::rename(replacement,&path).unwrap();
    let batch=tail.poll(&path).unwrap();assert!(batch.reset);assert_eq!(batch.lines,["replaced"]);
}
#[test]
fn tail_oversized_line_is_bounded_and_recovers_across_polls() {
    let tmp=Temp::new();let path=tmp.0.join("tail");fs::write(&path,vec![b'a';tail::MAX_LINE+10]).unwrap();
    let mut tail=Tailer::new();let mut skipped=0;
    loop {let b=tail.poll(&path).unwrap();assert!(b.lines.is_empty());skipped+=b.skipped;if !b.has_more {break;}}
    assert_eq!(skipped,1);append(&path,b"more\nok\n");let b=tail.poll(&path).unwrap();assert_eq!(b.lines,["ok"]);assert_eq!(b.skipped,0);
}
#[test]
fn discovery_cwd_dates_invalid_header_and_size() {
    let tmp=Temp::new();let now=time::parse_ms("2026-11-07T12:00:00Z").unwrap();let day=(now/86400000)as i64;
    fixture_session(&tmp.0,day,"match","/repo","2026-11-07T10:00:00Z");
    fixture_session(&tmp.0,day-1,"prior","/repo","2026-11-06T10:00:00Z");
    fixture_session(&tmp.0,day,"other","/other","2026-11-07T09:00:00Z");
    fixture_session(&tmp.0,day-3,"old","/repo","2026-11-04T10:00:00Z");
    let bad=fixture_session(&tmp.0,day,"bad","/repo","2026-11-07T10:00:00Z");fs::write(&bad,b"{\"type\":\"turn_context\"}\n").unwrap();
    let huge=fixture_session(&tmp.0,day,"huge","/repo","2026-11-07T10:00:00Z");fs::write(&huge,vec![b'x';tail::MAX_LINE+1]).unwrap();
    let found=discover::find_sessions_at(&tmp.0,Some("/repo"),0,now);assert_eq!(found.iter().map(|s|s.thread_id.as_str()).collect::<Vec<_>>(),["prior","match"]);
    assert_eq!(discover::find_sessions_at(&tmp.0,Some("/repo"),now-3600*1000*3,now).len(),1);
    assert!(discover::find_sessions_at(&tmp.0,Some("/missing"),0,now).is_empty());
}
#[cfg(unix)]
#[test]
fn discovery_canonicalizes_existing_cwd_and_ignores_symlinks() {
    let tmp=Temp::new();let now=time::parse_ms("2026-11-07T12:00:00Z").unwrap();let day=(now/86400000)as i64;
    let cwd=tmp.0.join("cwd");fs::create_dir(&cwd).unwrap();let alias=tmp.0.join("alias");std::os::unix::fs::symlink(&cwd,&alias).unwrap();
    let p=fixture_session(&tmp.0,day,"canonical",cwd.to_str().unwrap(),"2026-11-07T10:00:00Z");
    std::os::unix::fs::symlink(&p,p.with_file_name("rollout-link.jsonl")).unwrap();
    assert_eq!(discover::find_sessions_at(&tmp.0,Some(alias.to_str().unwrap()),0,now).len(),1);
}
#[tokio::test]
async fn invalid_thread_ids_rejected_before_root_resolution() {
    for id in ["", "../foo", "abc/def", "..", "a.b", "中", "a\\b"] {
        assert!(load_session(id.into()).await.unwrap_err().contains("threadId"));
        assert!(load_codex_session_at(Path::new("/nonexistent-event-test-root"),id,0).unwrap_err().contains("threadId"));
    }
    assert!(valid_thread_id("abc-123-DEF"));
}
#[test]
fn load_uses_discovery_and_finishes_unterminated_json() {
    let tmp=Temp::new();let now=time::parse_ms("2026-11-07T12:00:00Z").unwrap();let day=(now/86400000)as i64;
    let path=fixture_session(&tmp.0,day,"fixture","/repo","2026-11-07T10:00:00Z");
    append(&path,CODEX.trim_end().as_bytes());
    let result=load_codex_session_at(&tmp.0,"fixture",now).unwrap();assert!(result.skipped>=2);assert_eq!(result.summary.agents[&Agent::Codex].usage.total,133349);
}
#[test]
fn watches_discover_tail_stop_and_shutdown_using_temp_only() {
    use std::{sync::mpsc,time::Duration};
    let tmp=Temp::new();let now=time::now_ms();let day=(now/86400000)as i64;
    let date=time::date_path(day).replace('/',"-");let ts=format!("{date}T00:00:00Z");
    let manager=watch::WatchManager::default();let (tx,rx)=mpsc::channel();
    let id=manager.start(tmp.0.clone(),"/repo".into(),move |e| {let _=tx.send(e);}).unwrap();
    let path=fixture_session(&tmp.0,day,"watchtest","/repo",&ts);
    let event=rx.recv_timeout(Duration::from_secs(3)).unwrap();assert_eq!(event.watch_id,id);assert_eq!(event.event.source_id,"watchtest");
    append(&path,b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"duration_ms\":1}}");
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());append(&path,b"\n");
    assert!(matches!(rx.recv_timeout(Duration::from_secs(3)).unwrap().event.kind,EventKind::TurnCompleted{..}));
    manager.stop(&id);assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
    let (tx,rx)=mpsc::channel();manager.start(tmp.0.clone(),"/repo".into(),move |e| {let _=tx.send(e);}).unwrap();
    rx.recv_timeout(Duration::from_secs(3)).unwrap();manager.shutdown();
    assert!(manager.start(tmp.0.clone(),"/repo".into(),|_|{}).is_err());
}
#[test]
fn dates_are_correct_and_malformed_dates_do_not_panic() {
    assert_eq!(time::parse_ms("1970-01-01T00:00:00Z"),Some(0));
    assert_eq!(time::parse_ms("2026-11-07T09:53:45.515Z"),Some(1794045225515));
    assert_eq!(time::parse_ms("2026-11-07T10:53:45.515+01:00"),Some(1794045225515));
    assert_eq!(time::date_path(0),"1970/01/01");
    for s in ["中文", "2026-02-30T00:00:00Z", "2026-11-07T99:00:00Z", "2026-11-07T00:00:00.", "2026-11-07T00:00:00+99:00"] {assert_eq!(time::parse_ms(s),None);}
}
#[test]
fn phases_readonly_late_context_resume_and_gate() {
    use summary::Phase;
    fn event(ts_ms:u64,agent:Agent,source:&str,kind:EventKind)->RunEvent {RunEvent{ts_ms,agent,source_id:source.into(),kind}}
    let mut s=RunSummary::default();
    let cmd=EventKind::Command{command:"pwd".into(),exit_code:Some(0),duration_ms:Some(1),output_tail:String::new(),status:"completed".into(),truncated:false};
    s.apply(&event(10,Agent::Codex,"ro",cmd.clone()));
    s.apply(&event(5,Agent::Codex,"ro",EventKind::Session{session:Session{thread_id:"ro".into(),sandbox:Some("read-only".into()),..Session::default()}}));
    assert_eq!(s.phase,Phase::ReReview); assert_eq!(s.phases[&Phase::ReReview].commands,1);
    for (ts,mode,round) in [(20,HandoffMode::Fresh,1),(30,HandoffMode::Fresh,1),(40,HandoffMode::Resume,2)] {
        s.apply(&event(ts,Agent::Claude,"c",EventKind::Handoff{mode,description:String::new()}));assert_eq!(s.round,round);
    }
    s.apply(&event(50,Agent::Codex,"rw",cmd.clone()));assert_eq!(s.phase,Phase::Impl);
    s.apply(&event(51,Agent::Codex,"rw",EventKind::TurnCompleted{duration_ms:Some(1),last_message:String::new(),truncated:false}));
    s.apply(&event(52,Agent::Claude,"c",EventKind::Message{role:"assistant".into(),phase:None,text:"review".into(),truncated:false}));assert_eq!(s.phase,Phase::Review);
    s.apply(&event(60,Agent::Claude,"c",EventKind::RunResult{status:"success".into(),cost_usd:0.0,duration_ms:1,num_turns:1,per_model:Default::default()}));assert_eq!(s.phase,Phase::Gate);
    // Read-only thread activity is always reReview, including after a result.
    s.apply(&event(61,Agent::Codex,"ro",cmd));assert_eq!(s.phase,Phase::ReReview);
}
#[test]
fn rate_windows_and_command_duration_and_reasoning() {
    let events=claude_events().0;
    let windows=events.iter().filter_map(|e|if let EventKind::RateLimit{windows,..}=&e.kind{Some(windows)}else{None}).last().unwrap();
    assert_eq!(windows.len(),2);assert!((windows[0].used_percent-7.0).abs()<1e-9);assert_eq!(windows[1].used_percent,49.0);assert_eq!(windows[0].resets_at,Some(1794062400.0));
    let mut p=CodexParser::new();
    let event=p.feed_line(&json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","command":["sh","-c","fallback"],"duration":{"secs":2,"nanos":123000000},"exit_code":3,"aggregated_output":"end"}}}).to_string()).pop().unwrap();
    assert!(matches!(event.kind,EventKind::Command{command,duration_ms:Some(2123),exit_code:Some(3),..}if command=="fallback"));
    let event=p.feed_line(&json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"Reasoning","summary_text":["visible summary"],"raw_content":"private"}}}).to_string()).pop().unwrap();
    assert!(matches!(event.kind,EventKind::Message{text,phase:Some(phase),..} if text=="visible summary"&&phase=="reasoning"));
}
#[test]
fn sensitive_labeled_values_and_long_key_boundary_are_redacted() {
    let r=redact::Redactor::default();
    for text in ["accountId: 123456", "user_id=somebody", "system_prompt: private instructions"] {
        assert_eq!(r.clean(text),text);
    }
    assert_eq!(r.clean("Bearer   abc.def"),"Bearer [已隐藏]");
    // Redact before truncating, so a key crossing the truncation boundary is never exposed.
    let text=format!("{} sk-abcdefghijklmnopqrstuvwxyz123456 末尾", "中".repeat(3994));
    let (out,clipped)=r.text(&text);assert!(clipped);assert!(!out.contains("sk-"));
}
#[test]
fn redact_preserves_code_paths_and_only_matches_key_boundaries() {
    let r = redact::Redactor::default();
    for text in [
        "+ user_id = row['user_id']", "src/signature.py", "user_data = load()", "superuser_x",
        "userId account_id accountId system_prompt signature base_instructions encrypted_content",
        "creator_name account_name acct_balance", "<system>example</system>",
    ] {
        assert_eq!(r.clean(text), text);
        assert_eq!(r.diff(text), (text.to_owned(), false));
    }
    for prefix in ["sk-", "ghp_", "xoxb-"] {
        let key = format!("{prefix}abcdefghijklmnopqrstuvwxyz123456");
        for before in ["a", "1", "中"] {
            let text = format!("{before}{key}");
            assert_eq!(r.clean(&text), text);
        }
        for before in ["", " ", "(", "="] {
            assert_eq!(r.clean(&format!("{before}{key}")), format!("{before}[已隐藏]"));
        }
        let short = format!("{prefix}{}", "a".repeat(19));
        assert_eq!(r.clean(&short), short);
    }
    let mut parser = CodexParser::new();
    let event = parser.feed_line(&json!({"type":"event_msg","payload":{"type":"item_completed","item":{
        "type":"FileChange","id":"change","changes":{"src/signature.py":{"type":"update","unified_diff":"+ user_id = row['user_id']"}}
    }}}).to_string()).pop().unwrap();
    assert!(matches!(&event.kind, EventKind::FileChange { path, diff, .. }
        if path == "src/signature.py" && diff == "+ user_id = row['user_id']"));
}

#[test]
fn discovery_local_date_ahead_of_utc_and_rolling_time_bounds() {
    let tmp = Temp::new();
    let now = time::parse_ms("2026-11-07T23:00:00Z").unwrap();
    let day = (now / 86_400_000) as i64;
    fixture_session(&tmp.0, day + 1, "east", "/repo", "2026-11-07T23:00:00Z");
    fixture_session(&tmp.0, day - 3, "boundary", "/repo", "2026-11-04T23:00:00Z");
    fixture_session(&tmp.0, day - 3, "too-old", "/repo", "2026-11-04T22:59:59Z");
    fixture_session(&tmp.0, day + 1, "clock-skew", "/repo", "2026-11-07T23:01:00Z");
    fixture_session(&tmp.0, day + 1, "too-new", "/repo", "2026-11-07T23:01:01Z");
    let sessions = discover::find_sessions_at(&tmp.0, Some("/repo"), 0, now);
    assert_eq!(sessions.iter().map(|s| s.thread_id.as_str()).collect::<Vec<_>>(), ["boundary", "east", "clock-skew"]);
    assert_eq!(discover::find_sessions_at(&tmp.0, Some("/repo"), now, now).len(), 2);
}

#[test]
fn discovery_caches_matches_and_mismatches_but_retries_partial_headers() {
    let tmp = Temp::new();
    let now = time::parse_ms("2026-11-07T23:00:00Z").unwrap();
    let day = (now / 86_400_000) as i64;
    let ts = "2026-11-07T23:00:00Z";
    let matched = fixture_session(&tmp.0, day, "match", "/repo", ts);
    fixture_session(&tmp.0, day, "mismatch", "/other", ts);
    let partial = fixture_session(&tmp.0, day, "partial", "/repo", ts);
    fs::write(&partial, b"{\"type\":").unwrap();
    let mut cache = discover::DiscoveryCache::new(Some("/repo"));
    let first = cache.find_new_at(&tmp.0, 0, now);
    assert_eq!(first.len(), 1); assert_eq!(first[0].thread_id, "match");
    // Both confirmed paths are skipped even when their headers change to another match.
    fixture_session(&tmp.0, day, "mismatch", "/repo", ts);
    assert!(cache.find_new_at(&tmp.0, 0, now).is_empty());
    fs::write(&matched, b"not JSON\n").unwrap();
    fixture_session(&tmp.0, day, "partial", "/repo", ts);
    let next = cache.find_new_at(&tmp.0, 0, now);
    assert_eq!(next.len(), 1); assert_eq!(next[0].thread_id, "partial");
}

#[test]
fn watch_poll_cadence_summary_order_batching_and_replay() {
    use std::{cell::RefCell, time::{Duration, Instant}};
    use watch::WatchNotification;
    let tmp = Temp::new();
    let now = time::parse_ms("2026-11-07T23:00:00Z").unwrap();
    let day = (now / 86_400_000) as i64;
    let ts = "2026-11-07T23:00:00Z";
    let a = fixture_session(&tmp.0, day, "a", "/repo", ts);
    let mut poller = watch::WatchPoller::new(tmp.0.clone(), "/repo");
    let received = RefCell::new(Vec::new());
    let emit = |notification: WatchNotification<'_>| {
        let (name, payload) = match notification {
            WatchNotification::Event(e) => ("run-event", serde_json::to_value(e).unwrap()),
            WatchNotification::Reset(r) => ("run-reset", serde_json::to_value(r).unwrap()),
            WatchNotification::Summary(s) => ("run-summary", serde_json::to_value(s).unwrap()),
        };
        received.borrow_mut().push((name, payload));
    };
    let start = Instant::now();
    let mut tick = |ms| {
        assert!(poller.poll_at("watch-test", start + Duration::from_millis(ms), now + ms, &emit, || false));
        received.replace(Vec::new())
    };
    let first = tick(0);
    assert_eq!(first.iter().map(|(name,_)| *name).collect::<Vec<_>>(), ["run-event", "run-summary"]);
    assert_eq!(first[1].1["watchId"], "watch-test");
    let command = format!("{}\n", json!({"type":"event_msg","timestamp":ts,"payload":{"type":"item_completed","item":{"type":"CommandExecution","command":["pwd"],"status":"completed"}}}));
    append(&a, command.repeat(2).as_bytes());
    let b = fixture_session(&tmp.0, day, "b", "/repo", ts);
    append(&b, command.as_bytes());
    let next = tick(400);
    assert_eq!(next.iter().map(|(name,_)| *name).collect::<Vec<_>>(), ["run-event", "run-event", "run-summary"]);
    assert_eq!(next.last().unwrap().1["summary"]["agents"]["codex"]["commands"], 2);
    assert!(next[..2].iter().all(|(_,e)| e["event"]["sourceId"] == "a"));
    for ms in [800, 1200, 1600] { assert!(tick(ms).is_empty()); }
    // Discovery at 2 seconds finds b. Its session and command precede one summary.
    let discovered = tick(2000);
    assert_eq!(discovered.iter().map(|(name,_)| *name).collect::<Vec<_>>(), ["run-event", "run-event", "run-summary"]);
    assert_eq!(discovered.last().unwrap().1["summary"]["agents"]["codex"]["commands"], 3);
    // All files in the same poll still produce only one combined summary.
    append(&a, command.as_bytes()); append(&b, command.as_bytes());
    let both = tick(2400);
    assert_eq!(both.iter().map(|(name,_)| *name).collect::<Vec<_>>(), ["run-event", "run-event", "run-summary"]);
    assert_eq!(both.last().unwrap().1["summary"]["agents"]["codex"]["commands"], 5);
    // Truncation removes a's earlier contribution without losing b's two commands.
    fixture_session(&tmp.0, day, "a", "/repo", ts);
    let reset = tick(2800);
    assert_eq!(reset.last().unwrap().1["summary"]["agents"]["codex"]["commands"], 2);
    assert!(tick(3200).is_empty());
}

#[test]
fn watch_worker_emits_summary_notifications_using_temp_only() {
    use std::{sync::mpsc, time::Duration};
    let tmp = Temp::new();
    let day = (time::now_ms() / 86_400_000) as i64;
    let ts = format!("{}T00:00:00Z", time::date_path(day).replace('/', "-"));
    fixture_session(&tmp.0, day, "worker", "/repo", &ts);
    let manager = watch::WatchManager::default();
    let (tx, rx) = mpsc::channel();
    let id = manager.start_with_notifications(tmp.0.clone(), "/repo".into(), move |notification| {
        let value = match notification {
            watch::WatchNotification::Event(event) => ("run-event", serde_json::to_value(event).unwrap()),
            watch::WatchNotification::Reset(reset) => ("run-reset", serde_json::to_value(reset).unwrap()),
            watch::WatchNotification::Summary(summary) => ("run-summary", serde_json::to_value(summary).unwrap()),
        };
        tx.send(value).unwrap();
    }).unwrap();
    let event = rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let summary = rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(event.0, "run-event"); assert_eq!(summary.0, "run-summary");
    assert_eq!(summary.1["watchId"], id); assert!(summary.1["summary"].is_object());
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
    manager.stop(&id);
}

#[test]
#[ignore = "仅审查方显式运行：读取真实最近会话；只打印统计信息，不打印事件文本"]
fn real_recent_sessions() {
    let root = discover::default_root().expect("无法确定会话目录");
    let now = time::now_ms();
    for info in discover::find_sessions_at(&root, None, now.saturating_sub(3 * 86_400_000), now) {
        let loaded = load_codex_session_at(&root, &info.thread_id, now).expect("无法加载会话统计");
        let commands = loaded.events.iter().filter(|event| matches!(event.kind, EventKind::Command { .. })).count();
        let files = loaded.events.iter().filter(|event| matches!(event.kind, EventKind::FileChange { .. })).count();
        let hidden = serde_json::to_string(&loaded.events).expect("无法序列化事件统计").matches("[已隐藏]").count();
        let codex = &loaded.summary.agents[&Agent::Codex];
        // Only the requested short session identifier, originator label and aggregate statistics.
        // Debug formatting keeps originator on one line; no event/output/command/diff is printed.
        println!("{} originator={:?} events={} skipped={} commands={} fileChanges={} total={} fresh={} phase={:?} round={} hidden={}",
            info.thread_id.chars().take(8).collect::<String>(), info.originator.as_deref().unwrap_or("unknown"),
            loaded.events.len(), loaded.skipped, commands, files, codex.usage.total, codex.usage.fresh,
            loaded.summary.phase, loaded.summary.round, hidden);
    }
}

#[test]
fn verdict_lines_main_scope_only_and_ordered() {
    let mut parser = claude::ClaudeParser::new();
    let frame = |parent: serde_json::Value, text: &str| json!({"type":"assistant","parent_tool_use_id":parent,"message":{"content":[{"type":"text","text":text}]}}).to_string();
    let events = parser.feed_line(&frame(serde_json::Value::Null, "审查\n[[verdict:start]]\n裁决说明\n[[verdict:fix]]\n继续"));
    assert_eq!(events.len(), 5);
    assert!(matches!(events[1].kind, EventKind::Verdict { outcome: VerdictOutcome::Start }));
    assert!(matches!(events[3].kind, EventKind::Verdict { outcome: VerdictOutcome::Fix }));
    for event in &events { if let EventKind::Message { text, .. } = &event.kind { assert!(!text.contains("[[verdict:")); } }
    for (parent, text) in [(json!(null), "inline [[verdict:pass]] trailing"), (json!("tool"), "[[verdict:pass]]")] {
        let events = parser.feed_line(&frame(parent, text));
        assert!(matches!(&events[0].kind, EventKind::Message { text: value, .. } if value == text));
        assert!(!events.iter().any(|e| matches!(e.kind, EventKind::Verdict { .. })));
    }
    let events = parser.feed_line(&json!({"type":"user","message":{"content":[{"type":"tool_result","content":"[[verdict:pass]]"}]}}).to_string());
    assert!(matches!(events[0].kind, EventKind::ToolResult { .. }));
    for (text, outcome) in [("[[verdict:pass]]", VerdictOutcome::Pass), ("[[verdict:fail]]", VerdictOutcome::Fail)] {
        assert!(matches!(parser.feed_line(&frame(json!(null), text))[0].kind, EventKind::Verdict { outcome: actual } if actual == outcome));
    }
}

#[test]
fn verdict_phase_fix_handoff_and_codex_without_round() {
    use summary::Phase;
    let mut summary = RunSummary::default();
    let mut apply = |agent, kind| {
        summary.apply(&RunEvent { agent, kind, source_id: "test".into(), ts_ms: 1 });
        (summary.phase, summary.round, summary.last_verdict)
    };
    let message = || EventKind::Message { role: "assistant".into(), phase: None, text: "activity".into(), truncated: false };
    assert_eq!(apply(Agent::Codex, message()).0, Phase::Impl);
    apply(Agent::Claude, EventKind::Handoff { mode: HandoffMode::Fresh, description: "".into() });
    assert_eq!(apply(Agent::Claude, EventKind::Verdict { outcome: VerdictOutcome::Start }).0, Phase::Verdict);
    assert_eq!(apply(Agent::Claude, message()).0, Phase::Verdict);
    apply(Agent::Claude, EventKind::Verdict { outcome: VerdictOutcome::Fix });
    let (phase, round, _) = apply(Agent::Claude, EventKind::Handoff { mode: HandoffMode::Resume, description: "".into() });
    assert_eq!((phase, round), (Phase::Impl, 2));
    for outcome in [VerdictOutcome::Pass, VerdictOutcome::Fail] {
        assert_eq!(apply(Agent::Claude, EventKind::Verdict { outcome }).0, Phase::Verdict);
        assert_eq!(apply(Agent::Claude, message()).0, Phase::Verdict);
    }
    assert_eq!(apply(Agent::Claude, EventKind::RunResult { status: "success".into(), cost_usd: 0.0, duration_ms: 0, num_turns: 1, per_model: Default::default() }).0, Phase::Gate);
}

#[test]
fn watch_reset_notifications_and_since_filter() {
    use watch::WatchUpdate;
    use std::time::Instant;
    let tmp = Temp::new();
    let now = time::parse_ms("2026-11-07T12:00:00Z").unwrap();
    let day = (now / 86400000) as i64;
    fixture_session(&tmp.0, day, "old", "/repo", "2026-11-07T11:00:00Z");
    let path = fixture_session(&tmp.0, day, "new", "/repo", "2026-11-07T12:00:00Z");
    let mut poller = watch::WatchPoller::new(tmp.0.clone(), "/repo").since(now);
    let events = poller.poll_updates(Instant::now(), now, || false);
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], WatchUpdate::Event(event) if event.source_id == "new"));
    fs::write(&path, "").unwrap();
    assert!(matches!(&poller.poll_updates(Instant::now(), now, || false)[0], WatchUpdate::Reset(id) if id == "new"));
    let replacement = tmp.0.join("replacement");
    fs::write(&replacement, "\n").unwrap(); fs::rename(replacement, &path).unwrap();
    assert!(matches!(&poller.poll_updates(Instant::now(), now, || false)[0], WatchUpdate::Reset(id) if id == "new"));
}
