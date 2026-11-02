use super::*;
use ratatui::{Frame,layout::{Rect,Position},style::{Style,Color,Modifier},text::{Line,Span},widgets::{Block,Borders,BorderType,Paragraph,Wrap}};
use editor::clean;

#[derive(Clone,Copy,PartialEq)]
pub(super) enum Screen { Chat,Email,Code,Account,Help,Detail,Memories }
#[derive(Clone,Copy,PartialEq)]
pub(super) enum Focus { Prompt,Tasks,Queue }
#[derive(Clone)]
pub(super) enum Hit { Memories,SaveMemories,ReloadMemories,Tasks,Access,Account,New,Stop,Prompt,Task(usize),Detail,StopProcess(usize),Choice(String),Custom,Queue(usize),Buy(String),Refresh,Logout,Back }

/// Neutral terminal colors remain readable without depending on the emulator's palette.
fn dim()->Style { Style::default().fg(Color::Gray) }
/// The current selection is visible in a strictly monochrome palette.
fn selected()->Style { Style::default().fg(Color::Black).bg(Color::White) }
/// Draw static text without interpreting provider-supplied terminal control codes.
fn label(frame:&mut Frame,area:Rect,text:impl Into<String>,style:Style) { frame.render_widget(Paragraph::new(clean(&text.into())).style(style),area); }
/// Wrap a block to the same width used for measuring and scrolling.
fn paragraph(frame:&mut Frame,area:Rect,lines:Vec<Line<'static>>,offset:usize) {
    let lines=lines.into_iter().map(|mut line| { for span in &mut line.spans { span.content=clean(&span.content).into(); } line }).collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim:false }).scroll((offset.min(u16::MAX as usize) as u16,0)),area);
}
/// Keep a one-line value inside its available terminal cells.
pub(super) fn short(text:&str,width:usize)->String {
    use unicode_width::UnicodeWidthStr;
    use unicode_segmentation::UnicodeSegmentation;
    let mut out=String::new(); let mut length=0;
    for g in clean(text).replace(['\n','\r']," ").graphemes(true) {
        if length+g.width()>width { break; }
        out.push_str(g); length+=g.width();
    }
    out
}
/// Minimal inline emphasis keeps markdown summaries readable without producing raw HTML.
fn inline(text:&str)->Line<'static> {
    let mut bold=false;
    Line::from(clean(text).split("**").map(|part| { let style=if bold { Style::default().add_modifier(Modifier::BOLD) } else { Style::default() }; bold=!bold; Span::styled(part.to_owned(),style) }).collect::<Vec<_>>())
}
/// Show an editor viewport around its cursor instead of growing the whole terminal layout.
fn edit(frame:&mut Frame,area:Rect,editor:&Editor,secret:bool,focus:bool,pending:bool) {
    let block=Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(if focus { Style::default() } else { dim() });
    let inner=block.inner(area); frame.render_widget(block,area);
    if inner.is_empty() { return; }
    let (lines,(x,y))=editor.wrapped(inner.width,secret);
    let start=y.saturating_sub(inner.height.saturating_sub(1));
    label(frame,inner,lines.into_iter().skip(start as usize).collect::<Vec<_>>().join("\n"),if pending { dim() } else { Style::default() });
    if focus && !pending { frame.set_cursor_position(Position::new(inner.x+x.min(inner.width-1),inner.y+y-start)); }
}

impl App {
    /// Record interactive rectangles from the current frame, so resize never leaves stale hit targets.
    fn button(&mut self,frame:&mut Frame,area:Rect,text:&str,hit:Hit,active:bool) {
        label(frame,area,text,if active { selected() } else { dim() }); self.hits.push((area,hit));
    }
    /// Layout B shares one footer row between Full access and keyboard hints.
    pub(super) fn draw(&mut self,frame:&mut Frame,client:&Client) {
        self.hits.clear();
        let size=frame.area();
        frame.render_widget(Block::default().style(Style::default().fg(Color::White).bg(Color::Black)),size);
        if size.width<35 || size.height<12 { label(frame,size,"Enlarge the terminal (35 x 12 minimum)\nCtrl+C to quit",dim()); return; }
        let top=Rect::new(1,0,size.width-2,2);
        frame.render_widget(Block::default().borders(Borders::BOTTOM).border_style(dim()),top);
        let brand=if size.width>=110 { format!("{} · {} · {}",self.name,self.view["state"]["platform"].as_str().unwrap_or_default(),self.view["state"]["shell"].as_str().unwrap_or_default()) } else { self.name.clone() };
        label(frame,Rect::new(2,0,size.width.saturating_sub(if self.authenticated() && size.width>=100 { 61 } else { 40 }),1),brand,Style::default().add_modifier(Modifier::BOLD));
        let balance=self.view["state"]["account"]["credits"].as_i64().map(|v|format!("{v} credits")).unwrap_or_else(||"Sign in".into());
        let count=self.tasks.iter().filter(|task|task["finished_at"].is_null()).count();
        let title=if count>0 { format!("Tasks {count}") } else { "Tasks".into() };
        let balance_width=balance.len().min(20) as u16;
        let right=size.width.saturating_sub(balance_width+2);
        self.button(frame,Rect::new(right,0,balance_width,1),&balance,Hit::Account,self.screen==Screen::Account);
        if self.authenticated() && size.width>=65 {
            self.button(frame,Rect::new(right-12,0,10,1),&title,Hit::Tasks,self.focus==Focus::Tasks);
            self.button(frame,Rect::new(right-24,0,10,1),"New chat",Hit::New,false);
            if size.width>=100 { self.button(frame,Rect::new(right-35,0,10,1),"Memories",Hit::Memories,self.screen==Screen::Memories); }
        }
        let footer=Rect::new(1,size.height-1,size.width-2,1);
        let access=if client.full_access.load(Ordering::SeqCst) { "[x] Full access" } else { "[ ] Full access" };
        self.button(frame,Rect::new(1,footer.y,15,1),access,Hit::Access,false);
        let hints=if size.width>=120 { "Enter send · Ctrl+Enter steer · F2 Tasks · F3 Access · F6 Credits · F10 Memories · F1 Help" }
            else if size.width>=95 { "Enter send · F5 steer · F2 Tasks · F3 Access · F10 Memories · F1 Help" }
            else if size.width>=75 { "Enter send · F2 Tasks · F10 Memories · F1 Help" }
            else { "F1 Help · F10 Memories" };
        label(frame,Rect::new(17,footer.y,size.width.saturating_sub(18),1),format!("· {hints}"),dim());
        let connection=self.view["connections"].as_object().and_then(|map|map.values().next()).map(|v|format!("Connecting to backend · attempt {}/5",v["attempt"]));
        let status=if !self.status.is_empty() { self.status.clone() } else if let Some(connection)=connection { connection } else if self.pending { "Please wait…".into() } else { self.view["backend_error"].as_str().or_else(||self.view["error"].as_str()).unwrap_or_default().into() };
        let mut body=Rect::new(1,2,size.width-2,size.height-3);
        if !status.is_empty() { label(frame,Rect::new(2,body.bottom()-1,body.width-2,1),status,dim()); body.height-=1; }
        match self.screen {
            Screen::Email|Screen::Code=>self.login_view(frame,body),
            Screen::Memories=>self.memories_view(frame,body),
            Screen::Help=>self.help_view(frame,body),
            Screen::Account=>self.account_view(frame,body),
            Screen::Detail=>self.detail_view(frame,body,client),
            Screen::Chat=> {
                if self.tasks_open && body.width>=98 {
                    let width=34;
                    self.chat_view(frame,Rect::new(body.x,body.y,body.width-width-1,body.height));
                    self.tasks_view(frame,Rect::new(body.right()-width,body.y,width,body.height));
                } else if self.tasks_open && self.focus==Focus::Tasks { self.tasks_view(frame,body); }
                else { self.chat_view(frame,body); }
            }
        }
    }
    /// Email sign-in uses the same six-digit verification as the browser client.
    fn login_view(&mut self,frame:&mut Frame,body:Rect) {
        let width=body.width.min(64);
        let area=Rect::new(body.x+(body.width-width)/2,body.y+body.height.saturating_sub(12)/2,width,body.height.min(12));
        frame.render_widget(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded),area);
        let x=area.x+2; let width=area.width.saturating_sub(4); let y=area.y+1;
        label(frame,Rect::new(x,y,width,1),if self.screen==Screen::Email { format!("Sign in to {}",self.name) } else { "Check your email".into() },Style::default().add_modifier(Modifier::BOLD));
        label(frame,Rect::new(x,y+2,width,2),if self.screen==Screen::Email { "Ask questions, investigate your system, and get tasks done.\nSign in with your email address.".into() } else { format!("Enter the six-digit code sent to\n{}",self.email) },dim());
        edit(frame,Rect::new(x,y+5,width,3),&self.input,false,true,self.pending);
        label(frame,Rect::new(x,y+8,width,1),if self.screen==Screen::Email { "Enter to sign in" } else { "Enter to verify · Esc to change email" },dim());
    }
    /// Wrap and scroll the transcript while keeping composer, queue and questions fixed below it.
    fn chat_view(&mut self,frame:&mut Frame,area:Rect) {
        let waiting=self.question.is_some();
        let input_height=if waiting { 3 } else { (self.input.wrapped(area.width.saturating_sub(2),false).0.len() as u16+2).clamp(4,7).min(area.height/2) };
        let queued=self.view["queue"].as_array().unwrap().len();
        let queue_height=if queued==0 { 0 } else { (queued.min(3)+1) as u16 };
        let question_height=if waiting { self.question_height(area.width).min(area.height.saturating_sub(input_height+queue_height+3)) } else { 0 };
        let transcript_height=area.height.saturating_sub(input_height+queue_height+question_height+1);
        self.chat_area=Rect::new(area.x+1,area.y,area.width.saturating_sub(2),transcript_height);
        let lines=self.transcript();
        let count=Paragraph::new(lines.clone()).wrap(Wrap { trim:false }).line_count(self.chat_area.width);
        self.chat_bottom=count.saturating_sub(transcript_height as usize);
        paragraph(frame,self.chat_area,lines,self.scroll.unwrap_or(self.chat_bottom).min(self.chat_bottom));
        let mut y=area.y+transcript_height;
        if queued>0 {
            label(frame,Rect::new(area.x+1,y,area.width-2,1),format!("Queued ({queued}) · Tab select · E edit · Del remove · F5 steer"),dim()); y+=1;
            let queue=self.view["queue"].as_array().unwrap().clone();
            let start=self.queue.saturating_sub(2);
            for (i,item) in queue.iter().enumerate().skip(start).take(3) {
                let text=format!("{} {}",if item["editing"]==true { "edit" } else { "≡" },short(item["text"].as_str().unwrap_or_default(),area.width.saturating_sub(8) as usize));
                self.button(frame,Rect::new(area.x+1,y,area.width-2,1),&text,Hit::Queue(i),self.focus==Focus::Queue && self.queue==i); y+=1;
            }
        }
        if question_height>0 { self.question_view(frame,Rect::new(area.x,y,area.width,question_height)); y+=question_height; }
        let prompt=Rect::new(area.x,y,area.width,input_height);
        edit(frame,prompt,&self.input,false,self.focus==Focus::Prompt && !waiting,self.pending);
        if let Some(files)=&self.attachments {
            label(frame,Rect::new(prompt.x+2,prompt.y,prompt.width.saturating_sub(4),1),format!(" {} attachment(s) · /clear-attachments to remove ",files.len()),dim());
        }
        self.hits.push((prompt,Hit::Prompt));
        if self.input.text.is_empty() { label(frame,Rect::new(prompt.x+2,prompt.y+1,prompt.width.saturating_sub(4),1),if waiting { "Your next task…" } else { "What should we work on?" },dim()); }
        let label=if self.view["busy"]==true { "[ Stop ]" } else { "" };
        if !label.is_empty() { self.button(frame,Rect::new(prompt.right()-10,prompt.bottom()-2,8,1),label,Hit::Stop,false); }
    }
    /// Preserve final answers and public summaries, without displaying raw tool protocol envelopes.
    fn transcript(&self)->Vec<Line<'static>> {
        let mut lines=Vec::new();
        let messages=self.view["state"]["messages"].as_array().unwrap();
        if messages.is_empty() { lines.push(Line::from("")); lines.push(Line::from("What should we work on?").style(Style::default().add_modifier(Modifier::BOLD))); lines.push(Line::from("Describe a task. Your assistant can run local commands.").style(dim())); }
        for (index,message) in messages.iter().enumerate() {
            for step in self.view["steps"].as_array().unwrap().iter().filter(|step|step["after"].as_u64()==Some(index as u64)) { self.progress_lines(&mut lines,step,false); }
            if message["role"]=="tool" { continue; }
            lines.push(Line::from(""));
            if message["role"]=="user" { lines.push(Line::from("You").style(dim())); }
            for attachment in message["attachments"].as_array().into_iter().flatten() { lines.push(Line::from(format!("[{}]",attachment["name"].as_str().unwrap_or("Attachment"))).style(dim())); }
            for text in message["text"].as_str().unwrap_or_default().lines() { lines.push(inline(text)); }
            for run in self.view["runs"].as_array().unwrap().iter().filter(|run|run["end"].as_u64()==Some(index as u64+1) && !run["finished_at"].is_null()) {
                let elapsed=run["finished_at"].as_u64().unwrap_or(0).saturating_sub(run["started_at"].as_u64().unwrap_or(0))/1000;
                let cost=run["credits"].as_i64().map(|v|format!(" · {v} credits")).unwrap_or_default();
                lines.push(Line::from(format!("{} · {elapsed}s{cost}",run["error"].as_str().unwrap_or("Completed"))).style(dim()));
                if !run["error"].is_null() { lines.extend(run["partial"].as_str().unwrap_or_default().lines().map(inline)); }
                for result in self.view["tool_results"].as_object().unwrap().values().filter(|result|result["image"]["generated"]==true && result["image"]["run"]==run["started_at"]) {
                    lines.push(Line::from(format!("Generated image: {}",result["image"]["path"].as_str().unwrap_or_default())).style(dim()));
                }
            }
        }
        if self.view["busy"]==true {
            for steer in self.view["steering"].as_array().into_iter().flatten().filter(|item|item["message_index"].is_null()) {
                lines.push(Line::from("You · steering").style(dim())); lines.extend(steer["text"].as_str().unwrap_or_default().lines().map(inline));
            }
            let progress=&self.view["activity"]["progress"];
            self.progress_lines(&mut lines,progress,true);
            for text in progress["text"].as_str().unwrap_or_default().lines() { lines.push(inline(text)); }
            for result in self.view["tool_results"].as_object().unwrap().values().filter(|result|result["image"]["generated"]==true && result["image"]["run"]==self.view["runs"].as_array().unwrap().last().unwrap()["started_at"]) {
                lines.push(Line::from(format!("Generated image: {}",result["image"]["path"].as_str().unwrap_or_default())).style(dim()));
            }
            let pulse=["·  ","·· ","···"][((now()/400)%3) as usize];
            lines.push(Line::from(format!("{pulse} {}",self.view["activity"]["label"].as_str().unwrap_or("Thinking…"))).style(dim()));
        }
        lines
    }
    /// Display only reasoning summaries, plans and tool status already published to the browser.
    fn progress_lines(&self,lines:&mut Vec<Line<'static>>,step:&Value,active:bool) {
        let summaries=step["summaries"].as_array().into_iter().flatten();
        for summary in summaries {
            let text=summary["text"].as_str().unwrap_or_default();
            if text.is_empty() { continue; }
            let prefix=if summary["kind"]=="plan" { "Plan" } else if summary["kind"]=="web_search" { "Search" } else if summary["kind"]=="reasoning" { "Thinking" } else { "Action" };
            if active || self.expanded { lines.push(Line::from(format!("  {prefix} · {}",clean(text).replace("**","").replace('\n'," "))).style(dim())); }
        }
        if !active && !self.expanded && step["summaries"].as_array().is_some_and(|items|!items.is_empty()) { lines.push(Line::from("  Worked on this task · F9 to show steps").style(dim())); }
    }
    /// Keep process controls within the current chat and use selection instead of horizontal scrolling.
    fn tasks_view(&mut self,frame:&mut Frame,area:Rect) {
        let block=Block::default().title(" Tasks ").borders(Borders::ALL).border_style(dim());
        let inner=block.inner(area); frame.render_widget(block,area);
        if inner.height<3 { return; }
        let visible=(inner.height as usize).saturating_sub(3)/3;
        let start=self.task.saturating_sub(visible.saturating_sub(1));
        for (index,task) in self.tasks.clone().iter().enumerate().skip(start).take(visible) {
            let y=inner.y+((index-start)*3) as u16;
            let running=task["finished_at"].is_null();
            let style=if self.focus==Focus::Tasks && index==self.task { selected() } else { Style::default() };
            label(frame,Rect::new(inner.x+1,y,inner.width.saturating_sub(2),1),short(task["command"].as_str().unwrap_or_default(),inner.width.saturating_sub(2) as usize),style);
            self.hits.push((Rect::new(inner.x,y,inner.width,2),Hit::Task(index)));
            let elapsed=task["finished_at"].as_u64().unwrap_or_else(now).saturating_sub(task["started_at"].as_u64().unwrap_or(0))/1000;
            label(frame,Rect::new(inner.x+1,y+1,inner.width.saturating_sub(9),1),format!("{} · {elapsed}s · PID {}",if running { "Running" } else { "Done" },task["pid"].as_u64().unwrap_or(0)),dim());
            if running { self.button(frame,Rect::new(inner.right()-7,y+1,6,1),"■ Stop",Hit::StopProcess(index),false); }
        }
        if self.tasks.is_empty() { label(frame,inner,"No commands in this chat",dim()); }
        label(frame,Rect::new(inner.x,inner.bottom()-2,inner.width,2),"↑↓ select · Enter details\nF8 Stop · Tab prompt",dim());
    }
    /// Captured output and executable arguments are fetched only when a command is opened.
    fn detail_view(&mut self,frame:&mut Frame,body:Rect,client:&Client) {
        let result=self.detail.context("No command selected").and_then(|id|client.processes.get(id));
        let record=match result { Ok(record)=>record,Err(error)=> { label(frame,body,error.to_string(),dim()); return; } };
        let mut lines=vec![Line::from("Command details").style(Style::default().add_modifier(Modifier::BOLD)),Line::from(""),Line::from(format!("Executable: {}",record.program)),Line::from(format!("Arguments: {:?}",record.arguments)),Line::from(format!("Directory: {}",record.proposal.cwd)),Line::from(format!("PID: {:?} · Exit: {:?}",record.pid,record.exit_code))];
        if let Some(error)=record.error { lines.push(Line::from(error)); }
        lines.push(Line::from(format!("\nStandard output · {} bytes",record.stdout_bytes)));
        lines.extend(clean(&record.stdout).lines().map(|line|Line::from(line.to_owned())));
        lines.push(Line::from(format!("\nStandard error · {} bytes",record.stderr_bytes)));
        lines.extend(clean(&record.stderr).lines().map(|line|Line::from(line.to_owned())));
        paragraph(frame,Rect::new(body.x+1,body.y,body.width-2,body.height-2),lines,self.detail_scroll);
        self.button(frame,Rect::new(body.x+1,body.bottom()-1,20,1),"Esc Back · F8 Stop",Hit::Back,false);
    }
    /// Numbered options submit on selection; explanations appear for the highlighted option.
    fn question_height(&self,width:u16)->u16 {
        let item=&self.view["interactions"][0];
        let question=&item["questions"][self.answers.len()];
        let text=if item["type"]=="tool" { serde_json::to_string(&item["action"]).unwrap() } else { question["question"].as_str().unwrap_or_default().into() };
        let rows=Paragraph::new(text).wrap(Wrap { trim:false }).line_count(width.saturating_sub(4)) as u16;
        rows.min(4)+question["options"].as_array().map_or(2,|items|items.len() as u16)+6
    }
    /// Render one question at a time without consuming space with every option description.
    fn question_view(&mut self,frame:&mut Frame,area:Rect) {
        let item=self.view["interactions"][0].clone();
        let title=if item["type"]=="tool" { " Allow local operation? " } else { " Question " };
        let block=Block::default().title(title).borders(Borders::ALL).border_type(BorderType::Rounded);
        let inner=block.inner(area); frame.render_widget(block,area);
        if inner.height<3 { return; }
        let question=&item["questions"][self.answers.len()];
        let text=if item["type"]=="tool" { serde_json::to_string_pretty(&item["action"]).unwrap() } else { question["question"].as_str().unwrap_or_default().into() };
        let height=(Paragraph::new(text.clone()).wrap(Wrap { trim:false }).line_count(inner.width.saturating_sub(2)) as u16).min(4).min(inner.height-2);
        paragraph(frame,Rect::new(inner.x+1,inner.y,inner.width-2,height),vec![inline(&text)],0);
        let options=if item["type"]=="tool" { vec![json!({"label":"Allow"}),json!({"label":"Deny"})] } else { question["options"].as_array().cloned().unwrap_or_default() };
        if !self.custom {
            if let Some(description)=options.get(self.choice).and_then(|option|option["description"].as_str()) {
                label(frame,Rect::new(inner.x+1,inner.bottom().saturating_sub(4),inner.width-2,1),short(description,inner.width.saturating_sub(2) as usize),dim());
            }
        }
        let start=self.choice.saturating_sub(inner.height.saturating_sub(height+4) as usize);
        let mut y=inner.y+height;
        for (index,option) in options.iter().enumerate().skip(start) {
            if y>=inner.bottom().saturating_sub(3) { break; }
            let label=option["label"].as_str().unwrap();
            self.button(frame,Rect::new(inner.x+1,y,inner.width-2,1),&format!("{}  {label}",index+1),Hit::Choice(label.into()),!self.custom && self.choice==index); y+=1;
        }
        if item["type"]!="tool" && (question["isOther"]==true || options.is_empty()) {
            let custom=Rect::new(inner.x,inner.bottom().saturating_sub(3),inner.width,3);
            edit(frame,custom,&self.answer,question["isSecret"]==true,self.custom || options.is_empty(),false);
            self.hits.push((custom,Hit::Custom));
            if self.answer.text.is_empty() { label(frame,Rect::new(custom.x+2,custom.y+1,custom.width.saturating_sub(4),1),"Tab: your own answer · Enter to send",dim()); }
        }
    }
    /// Current prices and the latest three successful purchases come from the backend.
    fn account_view(&mut self,frame:&mut Frame,body:Rect) {
        let mut y=body.y+1;
        label(frame,Rect::new(body.x+2,y,body.width-4,2),format!("Account & credits\n{}",self.view["state"]["account"]["email"].as_str().unwrap_or_default()),Style::default().add_modifier(Modifier::BOLD)); y+=3;
        self.button(frame,Rect::new(body.x+2,y,24,1),"R Refresh balance",Hit::Refresh,false); y+=2;
        label(frame,Rect::new(body.x+2,y,body.width-4,1),"Buy credits",dim()); y+=1;
        for (index,pack) in self.packs.clone().iter().enumerate().take(9) {
            if y>=body.bottom()-4 { break; }
            self.button(frame,Rect::new(body.x+2,y,body.width-4,1),&format!("{}  {} credits · ${} USD",index+1,pack["credits"],pack["usd"].as_str().unwrap_or_default()),Hit::Buy(pack["price_id"].as_str().unwrap().into()),false); y+=1;
        }
        if !self.purchases.is_empty() && y+3<body.bottom() {
            y+=1; label(frame,Rect::new(body.x+2,y,body.width-4,1),"Recent purchases",dim()); y+=1;
            for purchase in self.purchases.iter().take(3) {
                if y>=body.bottom()-2 { break; }
                let age=(now()/1000).saturating_sub(purchase["paid_at"].as_u64().unwrap_or(now()/1000));
                let time=if age>=86400 { format!("{} days ago",age/86400) } else if age>=3600 { format!("{} hours ago",age/3600) } else { format!("{} minutes ago",age/60) };
                label(frame,Rect::new(body.x+2,y,body.width-4,1),format!("+{} credits · {time}",purchase["credits"]),dim()); y+=1;
            }
        }
        self.button(frame,Rect::new(body.x+2,body.bottom()-2,20,1),"L Sign out",Hit::Logout,false);
        self.button(frame,Rect::new(body.x+2,body.bottom()-1,20,1),"Esc Back",Hit::Back,false);
    }
    /// A separate editor preserves the prompt draft and follows the cursor through terminal resizes.
    fn memories_view(&mut self,frame:&mut Frame,body:Rect) {
        label(frame,Rect::new(body.x+1,body.y+1,body.width-2,1),"Memories",Style::default().add_modifier(Modifier::BOLD));
        label(frame,Rect::new(body.x+1,body.y+2,body.width-2,1),"Remembered preferences and useful results from previous tasks",dim());
        edit(frame,Rect::new(body.x+1,body.y+4,body.width-2,body.height.saturating_sub(6)),&self.memories,false,self.memory_document.is_some(),self.pending);
        self.button(frame,Rect::new(body.x+1,body.bottom()-1,17,1),"Ctrl+S Save",Hit::SaveMemories,false);
        if body.width>=42 { self.button(frame,Rect::new(body.x+20,body.bottom()-1,18,1),"Ctrl+R Reload",Hit::ReloadMemories,false); }
        if body.width>=62 { self.button(frame,Rect::new(body.x+40,body.bottom()-1,20,1),"Esc / F10 Back",Hit::Back,false); }
    }

    /// List actions available in narrow terminals as well as the full workspace layout.
    fn help_view(&mut self,frame:&mut Frame,body:Rect) {
        let text="Keyboard shortcuts\n\nEnter              Send / queue / answer\nCtrl+Enter / F5     Steer draft, or first queued prompt\nShift/Alt+Enter     New line (Alt+Enter works in older terminals)\nF2                 Toggle Tasks\nF3                 Toggle Full access\nF4                 New chat\nF6                 Account & credits\nF7                 Retry unfinished request\nF8                 Stop selected process\nF9                 Show / hide completed steps\nF10                Open / close Memories\nCtrl+S / Ctrl+R    Save / reload in Memories\nTab                Prompt / Tasks / queue; custom question answer\nPage Up/Down       Scroll transcript; End jumps to latest\nEsc                Back; stop running conversation\nCtrl+C             Quit (stops the running conversation)\n\nQueue: ↑↓ select, E edit, Delete remove, F5 steer\nTasks: ↑↓ select, Enter details, F8 Stop\nQuestions: number or click an option; Tab for a custom answer\nMouse wheel        Scroll history or command output\n/attach PATH       Attach original image or UTF-8 file\n/clear-attachments Remove pending attachments\n\nEsc returns to your conversation";
        paragraph(frame,body,text.lines().map(|line|Line::from(line.to_owned())).collect(),self.detail_scroll);
    }
}

/// Process timing uses the same epoch milliseconds as the native command records.
fn now()->u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 }
