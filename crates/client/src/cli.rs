use std::{collections::HashMap,io::{self,IsTerminal},path::PathBuf,sync::{Arc,atomic::{AtomicBool,Ordering}},time::Duration};
use anyhow::{Context,Result,ensure};
use crossterm::{event::{Event,EventStream,KeyCode,KeyEventKind,KeyModifiers,MouseEventKind,EnableBracketedPaste,DisableBracketedPaste,EnableMouseCapture,DisableMouseCapture,KeyboardEnhancementFlags,PushKeyboardEnhancementFlags,PopKeyboardEnhancementFlags},execute};
use serde_json::{json,Value};
use tokio::sync::{Mutex,mpsc};
use tokio_stream::StreamExt;
use uuid::Uuid;
use crate::{controls::{Controls,Queued},engine::Engine,processes::Processes,stream::Live};
mod editor;
mod view;
use editor::Editor;
use view::{Screen,Focus,Hit};

#[derive(Clone)]
struct Client { engine:Arc<Mutex<Engine>>,controls:Arc<Controls>,processes:Processes,live:Live,full_access:Arc<AtomicBool>,auth:crate::session::Credentials }
#[derive(Clone)]
enum Action { Memories,SaveMemories(String,String),Login(String),Verify(String),Submit(Queued,bool),Steer(Uuid),Stop,New,Retry,Account,Refresh,Buy(String),Logout,Edit(Uuid),Remove(Uuid),Attach(String) }
struct Completed { action:Action,result:Result<Value> }
struct App {
    name:String,screen:Screen,focus:Focus,input:Editor,answer:Editor,view:Value,tasks:Vec<Value>,tasks_open:bool,
    task:usize,queue:usize,scroll:Option<usize>,chat_bottom:usize,detail_scroll:usize,detail:Option<Uuid>,pending:bool,status:String,expanded:bool,confirmed:bool,
    generation:protocol::GenerationOptions,email:String,editing:Option<Uuid>,attachments:Option<Vec<protocol::Attachment>>,question:Option<Uuid>,answers:HashMap<String,Vec<String>>,choice:usize,custom:bool,
    memories:Editor,memory_document:Option<Value>,packs:Vec<Value>,purchases:Vec<Value>,hits:Vec<(ratatui::layout::Rect,Hit)>,chat_area:ratatui::layout::Rect,
}

/// Own the assistant directly; terminal mode never binds a port or starts a browser service.
pub async fn run(backend:&str)->Result<()> {
    ensure!(io::stdin().is_terminal() && io::stdout().is_terminal(),"--cli needs an interactive terminal");
    let engine=Engine::open(backend).await?;
    let client=Client { controls:engine.controls.clone(),processes:engine.processes.clone(),live:engine.live.clone(),full_access:engine.full_access.clone(),auth:engine.auth.clone(),engine:Arc::new(Mutex::new(engine)) };
    let branding=tokio::spawn(crate::config::load(backend.into(),Arc::new(crate::config::Brand::default()),client.live.clone()));
    let mut app=App::new("Assistant",client.live.0.borrow().clone());
    let (send,mut completed)=mpsc::unbounded_channel();
    let mut snapshots=client.live.0.subscribe();
    let mut events=EventStream::new();
    let mut tick=tokio::time::interval(Duration::from_millis(100));
    let mut terminal=ratatui::init();
    let mut restore=TerminalGuard { enhanced:false };
    execute!(io::stdout(),EnableBracketedPaste,EnableMouseCapture)?;
    // Other terminals can use F5 to steer and Alt+Enter for a new line.
    let enhanced=crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced { execute!(io::stdout(),PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?; restore.enhanced=true; }
    if app.authenticated() { app.dispatch(&client,&send,Action::Refresh); }
    let result=async {
        loop {
            app.tasks=client.processes.list().as_array().unwrap().iter().filter(|task| {
                app.view["commands"].as_array().unwrap().iter().any(|command|command["id"]==task["id"])
            }).cloned().collect();
            app.task=app.task.min(app.tasks.len().saturating_sub(1));
            terminal.draw(|frame|app.draw(frame,&client))?;
            tokio::select! {
                Some(event)=events.next()=> {
                    let event=event?;
                    if matches!(event,Event::Key(key) if key.kind!=KeyEventKind::Release && key.modifiers.contains(KeyModifiers::CONTROL) && key.code==KeyCode::Char('c')) { break; }
                    if let Err(error)=app.event(event,&client,&send).await { app.status=error.to_string(); }
                }
                Some(done)=completed.recv()=>app.completed(done),
                result=snapshots.changed()=> { result?; app.update(snapshots.borrow_and_update().clone()); }
                _=tick.tick()=>{}
            }
        }
        Ok::<_,anyhow::Error>(())
    }.await;
    branding.abort();
    client.controls.cancel_local();
    client.processes.stop_foreground(None);
    let _=tokio::time::timeout(Duration::from_secs(3),client.controls.interrupt()).await;
    let _=tokio::time::timeout(Duration::from_secs(3),client.engine.lock()).await;
    result
}

struct TerminalGuard { enhanced:bool }
impl Drop for TerminalGuard {
    /// Restore the caller's shell on every normal and error exit.
    fn drop(&mut self) {
        if self.enhanced { let _=execute!(io::stdout(),PopKeyboardEnhancementFlags); }
        let _=execute!(io::stdout(),DisableBracketedPaste,DisableMouseCapture); ratatui::restore();
    }
}

impl Client {
    /// Queue edits and deletions may resume an idle worker, but must respect an explicit Stop.
    fn wake(&self,automatic:bool) {
        let view=self.live.0.borrow().clone();
        if view["busy"]==true || view["state"]["retry_available"]==true { return; }
        if automatic && self.controls.stopped.load(Ordering::SeqCst) { return; }
        let engine=self.engine.clone();
        tokio::spawn(async move { let _=crate::session::run(engine,false).await; });
    }
    /// Execute UI operations without blocking rendering or the terminal's input stream.
    async fn action(&self,action:&Action)->Result<Value> {
        if matches!(action,Action::Login(_)) { ensure!(!self.live.0.borrow()["brand"].is_null(),"Wait for backend configuration before signing in"); }
        match action {
            Action::Memories=>return crate::session::account(&self.auth,&self.live,reqwest::Method::GET,"/v1/memories",None).await,
            Action::SaveMemories(text,revision)=>return crate::session::account(&self.auth,&self.live,reqwest::Method::POST,"/v1/memories",Some(json!({"text":text,"revision":revision}))).await,
            Action::Login(email)=>return Ok(serde_json::to_value(self.engine.try_lock()?.login(email.clone()).await?)?),
            Action::Verify(code)=>return self.engine.try_lock()?.verify_login(code.clone()).await,
            Action::Submit(input,steer)=> {
                if *steer {
                    let input=protocol::Steering { id:input.id,text:input.text.clone(),attachments:input.attachments.clone() };
                    self.controls.steer(&input).await?;
                    crate::session::record_steering(&self.live,&input);
                } else {
                    self.controls.enqueue(input.clone()).await?;
                    self.wake(false);
                }
            }
            Action::Steer(id)=> { let input=self.controls.steer_queued(*id).await?; crate::session::record_steering(&self.live,&input); }
            Action::Stop=>crate::session::interrupt(&self.controls,&self.live,&self.processes).await?,
            Action::New=>self.engine.try_lock()?.new_chat().await?,
            Action::Retry=> { let engine=self.engine.clone(); tokio::spawn(async move { let _=crate::session::run(engine,true).await; }); }
            Action::Refresh=>return self.engine.try_lock()?.refresh_account().await,
            Action::Logout=>self.engine.try_lock()?.logout().await?,
            Action::Edit(id)=>return Ok(serde_json::to_value(self.controls.edit(*id,true).await?)?),
            Action::Remove(id)=> { self.controls.remove(*id).await?; self.wake(true); }
            Action::Attach(path)=> {
                use base64::Engine as _;
                let path=PathBuf::from(path.trim().trim_matches('"'));
                ensure!(tokio::fs::metadata(&path).await?.len()<protocol::MAX_UPLOAD_BYTES as u64/4*3,"Attachment exceeds the transport budget");
                let bytes=tokio::fs::read(&path).await?;
                let name=path.file_name().context("Missing file name")?.to_string_lossy().into_owned();
                let image=match image::guess_format(&bytes).ok() {
                    Some(image::ImageFormat::Png)=>Some("image/png"),Some(image::ImageFormat::Jpeg)=>Some("image/jpeg"),Some(image::ImageFormat::WebP)=>Some("image/webp"),_=>None
                };
                let attachment=if let Some(mime)=image { protocol::Attachment::Image { name,content:format!("data:{mime};base64,{}",base64::engine::general_purpose::STANDARD.encode(bytes)) } }
                    else { protocol::Attachment::Text { name,content:String::from_utf8(bytes).context("Attach UTF-8 text, PNG, JPEG or WebP files")? } };
                protocol::validate_input("",Some(std::slice::from_ref(&attachment)))?;
                return Ok(serde_json::to_value(attachment)?);
            }
            Action::Account=> {
                let packs=crate::session::account(&self.auth,&self.live,reqwest::Method::GET,"/v1/credit-packs",None).await?;
                let purchases=crate::session::account(&self.auth,&self.live,reqwest::Method::GET,"/v1/purchases",None).await?;
                return Ok(json!({"packs":packs["packs"],"purchases":purchases["purchases"]}));
            }
            Action::Buy(id)=> {
                let value=crate::session::account(&self.auth,&self.live,reqwest::Method::POST,"/v1/checkout",Some(json!({"price_id":id}))).await?;
                let url=value["url"].as_str().context("Checkout URL is missing")?;
                let parsed=reqwest::Url::parse(url)?;
                ensure!(parsed.scheme()=="https" && parsed.host_str()==Some("checkout.stripe.com"),"Unexpected checkout URL");
                webbrowser::open(url)?;
            }
        }
        Ok(Value::Null)
    }
}

impl App {
    /// Start at email sign-in for a fresh in-memory session.
    fn new(name:&str,view:Value)->Self {
        let screen=if view["state"]["authenticated"]==true { Screen::Chat } else { Screen::Email };
        Self { generation:Default::default(),name:name.into(),screen,focus:Focus::Prompt,input:Editor::default(),answer:Editor::default(),view,tasks:vec![],tasks_open:true,task:0,queue:0,scroll:None,chat_bottom:0,detail_scroll:0,detail:None,pending:false,status:String::new(),expanded:false,confirmed:false,email:String::new(),editing:None,attachments:None,question:None,answers:HashMap::new(),choice:0,custom:false,memories:Editor::default(),memory_document:None,packs:vec![],purchases:vec![],hits:vec![],chat_area:ratatui::layout::Rect::default() }
    }
    /// Authentication comes from the engine, never from terminal display modes.
    fn authenticated(&self)->bool { self.view["state"]["authenticated"]==true }
    /// Spawn short UI transactions; the shared worker owns long AI turns separately.
    fn dispatch(&mut self,client:&Client,send:&mpsc::UnboundedSender<Completed>,action:Action) {
        if self.pending { return; }
        self.pending=true;
        self.status.clear();
        let (client,send)=(client.clone(),send.clone());
        tokio::spawn(async move {
            let mut result=client.action(&action).await;
            if let Some(expired)=result.as_ref().err().and_then(|error|error.downcast_ref::<crate::backend::SessionExpired>()).cloned() {
                if let Err(error)=crate::session::expire(&client.engine,&client.controls,&client.auth,&expired).await { result=Err(error); }
            }
            let _=send.send(Completed { action,result });
        });
    }
    /// Keep failed drafts intact and transition login only after the backend accepts it.
    fn completed(&mut self,done:Completed) {
        self.pending=false;
        let value=match done.result { Ok(value)=>value,Err(error)=> { self.status=error.to_string(); return; } };
        match done.action {
            Action::Memories|Action::SaveMemories(_,_)=> {
                if !self.authenticated() { return; }
                self.memories=Editor::new(value["text"].as_str().unwrap()); self.memory_document=Some(value);
                if matches!(done.action,Action::SaveMemories(_, _)) { self.status="Memories saved".into(); }
            }
            Action::Login(email)=> {
                self.email=email; self.input=Editor::default(); self.screen=Screen::Code;
                if let Some(code)=value["development_code"].as_str() { self.status=format!("Development sign-in code: {code}"); }
            }
            Action::Verify(_)=> { self.screen=Screen::Chat; self.input=Editor::default(); }
            Action::Submit(_,_)=> { self.input=Editor::default(); self.attachments=None; self.editing=None; self.confirmed=false; self.scroll=None; self.focus=Focus::Prompt; }
            Action::New=> { self.input=Editor::default(); self.attachments=None; self.editing=None; self.scroll=None; self.screen=Screen::Chat; self.focus=Focus::Prompt; }
            Action::Account=> { self.packs=value["packs"].as_array().cloned().unwrap_or_default(); self.purchases=value["purchases"].as_array().cloned().unwrap_or_default(); }
            Action::Buy(_)=>self.status="Checkout opened in your browser. Press R after paying to refresh your balance.".into(),
            Action::Logout=> { self.memories=Editor::default(); self.memory_document=None; self.screen=Screen::Email; self.input=Editor::default(); self.packs.clear(); self.purchases.clear(); }
            Action::Edit(id)=> {
                self.input=Editor::new(value["text"].as_str().unwrap_or_default()); self.editing=Some(id);
                self.generation=serde_json::from_value(value["generation"].clone()).unwrap();
                self.attachments=serde_json::from_value(value["attachments"].clone()).unwrap_or(None); self.focus=Focus::Prompt;
            }
            Action::Attach(_)=> { self.attachments.get_or_insert_with(Vec::new).push(serde_json::from_value(value).unwrap()); self.input=Editor::default(); self.confirmed=false; }
            _=>{}
        }
    }
    /// Retain input focus while new public progress, approvals and question batches arrive.
    fn update(&mut self,view:Value) {
        if let Some(name)=view["brand"]["name"].as_str() { self.name=name.into(); }
        if self.authenticated() && view["state"]["authenticated"]!=true { self.screen=Screen::Email; self.input=Editor::default(); self.memories=Editor::default(); self.memory_document=None; }
        self.view=view;
        let id=self.view["interactions"].as_array().and_then(|items|items.first()).and_then(|item|item["id"].as_str()).and_then(|id|id.parse().ok());
        if self.question!=id { self.question=id; self.answers.clear(); self.answer=Editor::default(); self.choice=0; self.custom=false; }
    }
    /// Ctrl+Enter steers the draft, or the queue head when the composer is empty.
    fn submit(&mut self,client:&Client,send:&mpsc::UnboundedSender<Completed>,steer:bool)->Result<()> {
        if self.pending { return Ok(()); }
        let text=self.input.text.trim().to_owned();
        if self.screen==Screen::Email { ensure!(!text.is_empty(),"Enter your email address"); self.dispatch(client,send,Action::Login(text)); return Ok(()); }
        if self.screen==Screen::Code { ensure!(text.len()==6 && text.bytes().all(|b|b.is_ascii_digit()),"Enter the six-digit code"); self.dispatch(client,send,Action::Verify(text)); return Ok(()); }
        ensure!(self.authenticated(),"Sign in first");
        // Prepared for later activation: no model override takes effect until the backend enables selection.
        if text=="/model" || text.starts_with("/model ") {
            let catalog:protocol::ModelCatalog=serde_json::from_value(self.view["brand"]["model_catalog"].clone())?;
            ensure!(catalog.enabled,"Model selection is not enabled");
            let arguments=text.split_whitespace().skip(1).collect::<Vec<_>>();
            if arguments.is_empty() { self.status=catalog.models.iter().map(|model|format!("{} ({})",model.id,model.reasoning_levels.join(", "))).collect::<Vec<_>>().join("; "); return Ok(()); }
            ensure!(arguments.len()<=2,"Use /model MODEL [REASONING] or /model default");
            if arguments==["default"] { self.generation=Default::default(); }
            else {
                let model=catalog.models.iter().find(|model|model.id==arguments[0]).context("This model is not available")?;
                let effort=arguments.get(1).map(|value|value.to_string());
                ensure!(effort.as_ref().is_none_or(|effort|model.reasoning_levels.contains(effort)),"This reasoning level is not available for the selected model");
                self.generation=protocol::GenerationOptions { model:Some(model.id.clone()),reasoning_effort:effort };
            }
            self.input=Editor::default(); self.status="Model preferences updated for subsequent prompts".into(); return Ok(());
        }
        if let Some(path)=text.strip_prefix("/attach ") { self.dispatch(client,send,Action::Attach(path.into())); return Ok(()); }
        if text=="/clear-attachments" { self.attachments=None; self.input=Editor::default(); self.confirmed=false; return Ok(()); }
        let credits=self.view["state"]["account"]["credits"].as_i64().context("Refresh your account balance first (F6, R)")?;
        ensure!(credits>0,"Add credits before starting a task or steering (F6)");
        if text.is_empty() && self.attachments.is_none() {
            if steer {
                if let Some(id)=self.view["queue"].as_array().and_then(|queue|queue.iter().find(|item|item["editing"]!=true)).and_then(|item|item["id"].as_str()) {
                    self.dispatch(client,send,Action::Steer(id.parse()?));
                }
            }
            return Ok(());
        }
        ensure!(self.view["state"]["retry_available"]!=true || self.view["busy"]==true,"Retry the unfinished request (F7) or start a new chat (F4)");
        protocol::validate_input(&text,self.attachments.as_deref())?;
        let tokens=text.chars().count()/4+self.attachments.iter().flatten().map(|file|match file { protocol::Attachment::Text { content,.. }=>content.chars().count()/4,protocol::Attachment::Image { .. }=>2000 }).sum::<usize>();
        let rate=self.view["state"]["account"]["input_credits_per_million"].as_f64().unwrap_or(0.0);
        let credits=(tokens as f64*rate/1_000_000.0).ceil().max(1.0) as i64;
        if credits>8 && !self.confirmed {
            self.confirmed=true;
            self.status=format!("This input may use around {credits} credits. Send again to confirm.");
            return Ok(());
        }
        let input=Queued { generation:self.generation.clone(),id:self.editing.unwrap_or_else(Uuid::new_v4),text,cwd:std::env::current_dir()?.to_string_lossy().into_owned(),editing:false,attachments:self.attachments.clone() };
        self.dispatch(client,send,Action::Submit(input,steer && self.view["busy"]==true));
        Ok(())
    }
    /// Answer each question once; only the complete batch reaches the engine.
    fn choose(&mut self,client:&Client,label:String)->Result<()> {
        let item=self.view["interactions"][0].clone();
        if item["type"]=="tool" {
            client.controls.respond(protocol::InteractionResponse { id:item["id"].as_str().unwrap().parse()?,answer:protocol::InteractionAnswer::Approval { approve:label=="Allow" } })?;
            self.update(client.live.0.borrow().clone());
            return Ok(());
        }
        let questions=item["questions"].as_array().context("No question is waiting")?;
        let question=&questions[self.answers.len()];
        ensure!(!label.trim().is_empty(),"Enter an answer");
        self.answers.insert(question["id"].as_str().unwrap().into(),vec![label]);
        if self.answers.len()==questions.len() {
            let answer=protocol::InteractionAnswer::Questions { answers:self.answers.clone() };
            if let Err(error)=client.controls.respond(protocol::InteractionResponse { id:item["id"].as_str().unwrap().parse()?,answer }) { self.answers.clear(); return Err(error); }
            self.update(client.live.0.borrow().clone());
        }
        self.answer=Editor::default(); self.choice=0; self.custom=false; self.focus=Focus::Prompt;
        Ok(())
    }
    /// Full access changes only after an explicit user action, even during generation.
    fn access(&mut self,client:&Client)->Result<()> {
        ensure!(self.authenticated() && self.view["state"]["settings"]["commands_enabled"]==true,"Sign in and enable local commands first");
        let enabled=!client.full_access.load(Ordering::SeqCst);
        client.full_access.store(enabled,Ordering::SeqCst);
        client.live.0.send_modify(|view|view["state"]["full_access"]=json!(enabled));
        if enabled { client.controls.approve_waiting(); }
        Ok(())
    }

    /// Route mouse and keyboard input through the same actions, including on resized terminals.
    async fn event(&mut self,event:Event,client:&Client,send:&mpsc::UnboundedSender<Completed>)->Result<()> {
        if let Event::Mouse(mouse)=event {
            match mouse.kind {
                MouseEventKind::ScrollUp=>self.scroll_by(4),
                MouseEventKind::ScrollDown=>self.scroll_by(-4),
                MouseEventKind::Down(crossterm::event::MouseButton::Left)=> {
                    let hit=self.hits.iter().rev().find(|(area,_)|area.contains((mouse.column,mouse.row).into())).map(|(_,hit)|hit.clone());
                    if let Some(hit)=hit { self.hit(hit,client,send)?; }
                }
                _=>{}
            }
            return Ok(());
        }
        if let Event::Paste(text)=event {
            if self.pending { return Ok(()); }
            self.confirmed=false;
            if self.screen==Screen::Code {
                self.input.insert(&text.chars().filter(char::is_ascii_digit).collect::<String>());
                if self.input.text.len()==6 { self.submit(client,send,false)?; }
            } else if self.screen==Screen::Memories { self.memories.insert(&text); }
            else if self.screen==Screen::Chat && self.question.is_some() {
                self.custom=true; self.answer.insert(&text);
            } else if matches!(self.screen,Screen::Chat|Screen::Email) { self.input.insert(&text); }
            return Ok(());
        }
        let Event::Key(key)=event else { return Ok(()); };
        if key.kind==KeyEventKind::Release { return Ok(()); }
        if !matches!(key.code,KeyCode::Enter|KeyCode::F(5)) { self.confirmed=false; }
        match key.code {
            KeyCode::F(1)=> { self.screen=Screen::Help; self.detail_scroll=0; return Ok(()); }
            KeyCode::F(2) if self.authenticated()=> { self.hit(Hit::Tasks,client,send)?; return Ok(()); }
            KeyCode::F(3)=> { self.access(client)?; return Ok(()); }
            KeyCode::F(4) if self.authenticated()=> { self.hit(Hit::New,client,send)?; return Ok(()); }
            KeyCode::F(6) if self.authenticated()=> { self.hit(Hit::Account,client,send)?; return Ok(()); }
            KeyCode::F(7) if self.authenticated() && self.view["busy"]!=true=> { self.dispatch(client,send,Action::Retry); return Ok(()); }
            KeyCode::F(8) if self.authenticated()=> { self.hit(Hit::StopProcess(self.task),client,send)?; return Ok(()); }
            KeyCode::F(10) if self.authenticated()=> { self.hit(Hit::Memories,client,send)?; return Ok(()); }
            KeyCode::F(9)=> { self.expanded=!self.expanded; return Ok(()); }
            KeyCode::Esc=> {
                self.status.clear();
                if self.screen==Screen::Code { self.screen=Screen::Email; self.input=Editor::new(&self.email); }
                else if matches!(self.screen,Screen::Help|Screen::Detail|Screen::Account|Screen::Memories) { self.screen=if self.authenticated() { Screen::Chat } else { Screen::Email }; self.focus=Focus::Prompt; }
                else if self.focus!=Focus::Prompt { self.focus=Focus::Prompt; }
                else if self.editing.is_some() {
                    let id=self.editing.take().unwrap();
                    client.controls.edit(id,false).await?; client.wake(true); self.input=Editor::default(); self.attachments=None;
                } else if self.view["busy"]==true { self.dispatch(client,send,Action::Stop); }
                return Ok(());
            }
            KeyCode::PageUp=> { self.scroll_by(10); return Ok(()); }
            KeyCode::PageDown=> { self.scroll_by(-10); return Ok(()); }
            _=>{}
        }
        if self.screen==Screen::Memories {
            if self.pending { return Ok(()); }
            match key.code {
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL)=>self.hit(Hit::SaveMemories,client,send)?,
                KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL)=>self.dispatch(client,send,Action::Memories),
                KeyCode::Enter=>self.memories.insert("\n"),
                _=>self.memories.key(key),
            }
            return Ok(());
        }
        if self.screen==Screen::Account {
            match key.code {
                KeyCode::Char('r')=>self.dispatch(client,send,Action::Refresh),
                KeyCode::Char('l') if self.view["busy"]!=true=>self.dispatch(client,send,Action::Logout),
                KeyCode::Char(c) if c.is_ascii_digit() && c!='0'=> {
                    if let Some(id)=self.packs.get(c as usize-'1' as usize).and_then(|pack|pack["price_id"].as_str()) { self.dispatch(client,send,Action::Buy(id.into())); }
                }
                _=>{}
            }
            return Ok(());
        }
        if matches!(self.screen,Screen::Detail|Screen::Help) {
            match key.code { KeyCode::Up=>self.scroll_by(1),KeyCode::Down=>self.scroll_by(-1),_=>{} }
            return Ok(());
        }
        if self.screen==Screen::Chat && self.question.is_some() {
            self.question_key(key,client)?;
            return Ok(());
        }
        if self.screen==Screen::Chat && key.code==KeyCode::Tab {
            self.focus=match self.focus {
                Focus::Prompt if self.tasks_open=>Focus::Tasks,
                Focus::Tasks if self.view["queue"].as_array().is_some_and(|items|!items.is_empty())=>Focus::Queue,
                _=>Focus::Prompt,
            };
            return Ok(());
        }
        if self.screen==Screen::Chat && self.focus==Focus::Tasks {
            match key.code {
                KeyCode::Up=>self.task=self.task.saturating_sub(1),
                KeyCode::Down=>self.task=(self.task+1).min(self.tasks.len().saturating_sub(1)),
                KeyCode::Enter=>self.hit(Hit::Detail,client,send)?,
                _=>{}
            }
            return Ok(());
        }
        if self.screen==Screen::Chat && self.focus==Focus::Queue {
            let items=self.view["queue"].as_array().unwrap();
            let id=items.get(self.queue).and_then(|item|item["id"].as_str()).and_then(|id|id.parse().ok());
            match key.code {
                KeyCode::Up=>self.queue=self.queue.saturating_sub(1),
                KeyCode::Down=>self.queue=(self.queue+1).min(items.len().saturating_sub(1)),
                KeyCode::Char('e') if self.input.text.is_empty()=> { if let Some(id)=id { self.dispatch(client,send,Action::Edit(id)); } }
                KeyCode::Delete=> { if let Some(id)=id { self.dispatch(client,send,Action::Remove(id)); } }
                KeyCode::F(5)|KeyCode::Enter=> { if let Some(id)=id { self.dispatch(client,send,Action::Steer(id)); } }
                _=>{}
            }
            return Ok(());
        }
        if self.pending { return Ok(()); }
        if key.code==KeyCode::F(5) { return self.submit(client,send,true); }
        if key.code==KeyCode::Enter {
            if key.modifiers.intersects(KeyModifiers::SHIFT|KeyModifiers::ALT) && self.screen==Screen::Chat { self.input.insert("\n"); }
            else { self.submit(client,send,key.modifiers.contains(KeyModifiers::CONTROL))?; }
        } else if self.screen==Screen::Code {
            if matches!(key.code,KeyCode::Char(c) if !c.is_ascii_digit()) { return Ok(()); }
            self.input.key(key);
            if self.input.text.len()==6 && matches!(key.code,KeyCode::Char(_)) { self.submit(client,send,false)?; }
        } else {
            if key.code==KeyCode::End && self.input.text.is_empty() { self.scroll=None; }
            self.input.key(key);
        }
        Ok(())
    }
    /// Keep transcript autoscroll paused until the user explicitly returns to the bottom.
    fn scroll_by(&mut self,delta:isize) {
        if self.screen==Screen::Chat {
            let offset=self.scroll.unwrap_or(self.chat_bottom).saturating_add_signed(-delta);
            self.scroll=if offset>=self.chat_bottom { None } else { Some(offset) };
        } else { self.detail_scroll=self.detail_scroll.saturating_add_signed(-delta); }
    }
    /// Single-click choices submit immediately; Tab exposes the optional custom-answer editor.
    fn question_key(&mut self,key:crossterm::event::KeyEvent,client:&Client)->Result<()> {
        let item=self.view["interactions"][0].clone();
        let question=&item["questions"][self.answers.len()];
        let options=if item["type"]=="tool" { vec![json!({"label":"Allow"}),json!({"label":"Deny"})] } else { question["options"].as_array().cloned().unwrap_or_default() };
        if options.is_empty() { self.custom=true; }
        let can_custom=item["type"]!="tool" && (question["isOther"]==true || options.is_empty());
        match key.code {
            KeyCode::Tab if can_custom=>self.custom=!self.custom,
            KeyCode::Up if !self.custom=>self.choice=self.choice.saturating_sub(1),
            KeyCode::Down if !self.custom=>self.choice=(self.choice+1).min(options.len().saturating_sub(1)),
            KeyCode::Enter=> {
                let label=if self.custom { self.answer.text.trim().to_owned() } else { options[self.choice]["label"].as_str().unwrap().into() };
                self.choose(client,label)?;
            }
            KeyCode::Char(c) if !self.custom && c.is_ascii_digit() && c!='0'=> {
                if let Some(option)=options.get(c as usize-'1' as usize) { self.choose(client,option["label"].as_str().unwrap().into())?; }
            }
            _ if self.custom=>self.answer.key(key),
            _=>{}
        }
        Ok(())
    }
    /// Mouse targets use the same guards as keyboard shortcuts and never change Full access implicitly.
    fn hit(&mut self,hit:Hit,client:&Client,send:&mpsc::UnboundedSender<Completed>)->Result<()> {
        match hit {
            Hit::Memories if self.authenticated()=> {
                if self.screen==Screen::Memories { self.screen=Screen::Chat; self.focus=Focus::Prompt; }
                else {
                    self.screen=Screen::Memories;
                    if self.memory_document.as_ref().is_some_and(|memory|memory["text"].as_str()!=Some(self.memories.text.as_str())) { return Ok(()); }
                    self.dispatch(client,send,Action::Memories);
                }
            }
            Hit::SaveMemories=> {
                ensure!(self.memories.text.len()<=16000,"Keep memories below 16000 UTF-8 bytes");
                let revision=self.memory_document.as_ref().and_then(|memory|memory["revision"].as_str()).context("Load memories before saving")?;
                self.dispatch(client,send,Action::SaveMemories(self.memories.text.clone(),revision.into()));
            }
            Hit::ReloadMemories=>self.dispatch(client,send,Action::Memories),
            Hit::Tasks=> { self.tasks_open=!self.tasks_open || self.focus!=Focus::Tasks; self.focus=if self.tasks_open { Focus::Tasks } else { Focus::Prompt }; self.screen=Screen::Chat; }
            Hit::Access=>self.access(client)?,
            Hit::Account if self.authenticated()=> { self.screen=Screen::Account; self.dispatch(client,send,Action::Account); }
            Hit::New=> { ensure!(self.view["busy"]!=true,"Stop the running conversation first (Esc)"); self.dispatch(client,send,Action::New); }
            Hit::Stop=>self.dispatch(client,send,Action::Stop),
            Hit::Prompt=>self.focus=Focus::Prompt,
            Hit::Task(index)=> { self.focus=Focus::Tasks; self.task=index; self.hit(Hit::Detail,client,send)?; }
            Hit::Detail=> { self.detail=self.tasks.get(self.task).and_then(|task|task["id"].as_str()).and_then(|id|id.parse().ok()); self.detail_scroll=0; self.screen=Screen::Detail; }
            Hit::StopProcess(index)=> { if let Some(id)=self.tasks.get(index).and_then(|task|task["id"].as_str()) { client.processes.stop(id.parse()?)?; } }
            Hit::Choice(label)=>self.choose(client,label)?,
            Hit::Custom=>self.custom=true,
            Hit::Queue(index)=> { self.focus=Focus::Queue; self.queue=index; }
            Hit::Buy(id)=>self.dispatch(client,send,Action::Buy(id)),
            Hit::Refresh=>self.dispatch(client,send,Action::Refresh),
            Hit::Logout=> { ensure!(self.view["busy"]!=true,"Stop the running task before signing out"); self.dispatch(client,send,Action::Logout); }
            Hit::Back=> { self.screen=if self.authenticated() { Screen::Chat } else { Screen::Email }; self.focus=Focus::Prompt; }
            _=>{}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Loading/saving notes preserves the prompt; late responses after sign-out cannot repopulate private text.
    #[test]
    fn memories_preserve_prompt_and_clear_on_sign_out() {
        let mut app=App::new("Assistant",json!({"state":{"authenticated":true}}));
        app.input=Editor::new("Unsent prompt 世界");
        app.completed(Completed { action:Action::Memories,result:Ok(json!({"text":"Remembered preference","revision":"1"})) });
        assert_eq!(app.input.text,"Unsent prompt 世界");
        assert_eq!(app.memories.text,"Remembered preference");
        app.completed(Completed { action:Action::SaveMemories("Edited".into(),"1".into()),result:Ok(json!({"text":"Edited","revision":"2"})) });
        assert_eq!(app.input.text,"Unsent prompt 世界");
        assert_eq!(app.memory_document.as_ref().unwrap()["revision"],"2");
        app.update(json!({"state":{"authenticated":false}}));
        app.completed(Completed { action:Action::Memories,result:Ok(json!({"text":"Stale private response","revision":"3"})) });
        assert!(app.memory_document.is_none()); assert!(app.memories.text.is_empty());
    }
}
