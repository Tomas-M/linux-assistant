'use strict';
const token=location.hash.slice(1) || sessionStorage.getItem('assistant-token');
if (location.hash)
{
    sessionStorage.setItem('assistant-token',token);
    history.replaceState(null,'',location.pathname+location.search);
}
const elements=Object.fromEntries([...document.querySelectorAll('[id]')].map(element=>[element.id,element]));
let steerId, state, view={busy:false,steps:[]}, busy=false, acting=false, loginTimer, polling=false, stopped=false, messageKey='', startedAt;
const eventController=new AbortController();
document.documentElement.dataset.theme=localStorage.getItem('assistant-theme') || 'light';
elements.theme.textContent=document.documentElement.dataset.theme==='dark'?'☼':'☾';
elements.theme.setAttribute('aria-label',document.documentElement.dataset.theme==='dark'?'Switch to light mode':'Switch to dark mode');

// Keep every operation on the authenticated loopback origin.
async function api(path,body)
{
    const response=await fetch('/api/'+path,{method:body===undefined?'GET':'POST',headers:{Authorization:'Bearer '+token,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
    const value=await response.json();
    if (!response.ok) throw new Error(value.error || 'Request failed');
    return value;
}

// Render a small Markdown subset with DOM nodes; HTML and links never become executable markup.
function renderAnswer(element,text)
{
    element.replaceChildren();
    text.split('```').forEach((section,index)=>
    {
        if (index%2)
        {
            const pre=document.createElement('pre');
            pre.textContent=section.replace(/^[\w+-]*\n/u,'');
            element.append(pre);
            return;
        }
        for (const paragraph of section.split(/\n\s*\n/u).filter(Boolean))
        {
            const lines=paragraph.split('\n'), list=lines.every(line=>/^\s*[-*] /u.test(line));
            const block=document.createElement(list?'ul':/^#{1,3} /u.test(paragraph)?'h3':'p');
            for (const line of list?lines:[paragraph.replace(/^#{1,3} /u,'')])
            {
                const target=list?document.createElement('li'):block;
                const value=list?line.replace(/^\s*[-*] /u,''):line;
                for (const part of value.split(/(\*\*[^*]+\*\*|\x60[^\x60]+\x60)/u))
                {
                    if (part.startsWith('**') && part.endsWith('**')) { const strong=document.createElement('strong'); strong.textContent=part.slice(2,-2); target.append(strong); }
                    else if (part.startsWith('\x60') && part.endsWith('\x60')) { const code=document.createElement('code'); code.textContent=part.slice(1,-1); target.append(code); }
                    else target.append(document.createTextNode(part));
                }
                if (list) block.append(target);
            }
            element.append(block);
        }
    });
}

// Model text, command output, and user input are displayed as text, never HTML.
function render()
{
    if (!state) return;
    busy=view.busy || acting;
    elements.setup.hidden=state.authenticated;
    elements.workspace.hidden=!state.authenticated;
    elements['process-toggle'].hidden=!state.authenticated;
    elements.welcome.hidden=state.messages.length>0;
    elements.backend.value=state.settings.backend;
    elements.character.value=state.settings.character || '';
    elements.commands.checked=state.settings.commands_enabled;
    elements['full-access'].checked=state.full_access;
    elements['full-access'].disabled=!state.settings.character || (busy && !state.full_access);
    elements['permission-note'].textContent=state.full_access?'Commands run automatically':state.settings.commands_enabled?'You approve every command':'Local commands are off';
    elements.platform.textContent=(state.platform==='windows'?'Windows · PowerShell':'Linux · sh')+' · Local workspace';
    elements.account.textContent=state.account?state.account.email+' · '+state.account.credits+' credits':'Sign in to your assistant account';
    elements['credit-balance'].textContent=state.account?(state.account.development?'Simulated AI · ':'')+state.account.credits+' credits':'';
    elements['settings-open'].textContent=state.account?Array.from(state.account.email)[0].toUpperCase():'○';
    for (const id of ['new','character','commands','login','backend','credits','buy','logout','approve','deny','retry'])
        elements[id].disabled=busy;
    renderComposer();
    elements.prompt.disabled=!state.settings.character;
    elements.prompt.placeholder=state.settings.character?'Ask anything, or describe a task…':'Choose your assistant character in settings to begin';
    elements.retry.hidden=!state.retry_available || busy;
    elements.proposal.hidden=!state.pending || busy;
    if (state.pending)
    {
        const proposal=state.pending.proposal;
        elements.command.textContent=proposal.command;
        elements.cwd.textContent=proposal.cwd;
        elements.reason.textContent=proposal.reason;
    }
    const steps=(view.steps || []).filter(step=>step.after<state.messages.length);
    const key=JSON.stringify([state.messages,steps]);
    if (key!==messageKey)
    {
        messageKey=key;
        elements.messages.replaceChildren();
        state.messages.forEach((message,index)=>
        {
            for (const step of steps.filter(step=>step.after===index && step.summaries.length))
            {
                const details=document.createElement('details'), summary=document.createElement('summary');
                details.className='past-progress';
                summary.textContent='Assistant progress'+(step.attempt>1?' · attempt '+step.attempt:'');
                details.append(summary);
                for (const part of step.summaries)
                {
                    const p=document.createElement('p');
                    p.textContent=part.text;
                    details.append(p);
                }
                elements.messages.append(details);
            }
            if (message.role==='tool')
            {
                const details=document.createElement('details'), summary=document.createElement('summary'), output=document.createElement('pre');
                details.className='tool-result';
                summary.textContent=message.text.startsWith('User approved')?'Local command started':'Local command result';
                output.textContent=message.text;
                details.append(summary,output);
                elements.messages.append(details);
                return;
            }
            const article=document.createElement('article');
            article.className=message.role;
            if (message.role==='assistant')
            {
                const label=document.createElement('div');
                label.className='assistant-label';
                label.textContent='✳ Assistant';
                article.append(label);
            }
            const text=document.createElement('div');
            text.className='answer';
            if (message.role==='assistant') renderAnswer(text,message.text);
            else text.textContent=message.text;
            article.append(text);
            elements.messages.append(article);
        });
    }
    const activity=view.activity;
    elements.activity.hidden=!activity || (!view.busy && !view.error);
    if (activity)
    {
        elements['activity-label'].textContent=view.error?'Interrupted · partial response':activity.label;
        elements.summaries.replaceChildren();
        for (const part of activity.progress?.summaries || [])
        {
            const p=document.createElement('p');
            p.textContent=part.text;
            elements.summaries.append(p);
        }
        renderAnswer(elements['stream-text'],activity.progress?.text || '');
        elements['stream-text'].classList.toggle('streaming',view.busy);
    }
    if (view.error) elements.status.textContent=view.error;
}

// Shared snapshots restore an active conversation after refresh without posting another request.
async function watch()
{
    while (!stopped)
    {
        try
        {
            const response=await fetch('/api/events',{headers:{Authorization:'Bearer '+token},signal:eventController.signal});
            if (!response.ok) throw new Error('Open the URL printed by the executable to authorize this tab.');
            elements.connection.textContent='Connected locally';
            const reader=response.body.getReader(), decoder=new TextDecoder();
            let buffer='';
            while (!stopped)
            {
                const result=await reader.read();
                if (result.done) throw new Error('The local connection was interrupted.');
                buffer+=decoder.decode(result.value,{stream:true}).replace(/\r/g,'');
                let end;
                while ((end=buffer.indexOf('\n\n'))!==-1)
                {
                    const frame=buffer.slice(0,end);
                    buffer=buffer.slice(end+2);
                    const data=frame.split('\n').filter(line=>line.startsWith('data:')).map(line=>line.slice(5).trimStart()).join('\n');
                    if (!data) continue;
                    const follow=window.scrollY+window.innerHeight>=document.documentElement.scrollHeight-100;
                    const next=JSON.parse(data);
                    if (next.busy && !view.busy) startedAt=Date.now();
                    if (!next.busy) startedAt=null;
                    view=next;
                    state=view.state;
                    render();
                    if (follow && state.messages.length) window.scrollTo({top:document.documentElement.scrollHeight});
                }

            }
        }
        catch (error)
        {
            if (stopped) break;
            elements.connection.textContent='Reconnecting…';
            if (!state) elements.status.textContent=error.message;
            await new Promise(resolve=>setTimeout(resolve,1500));
        }
    }
}

// Mutations stay serialized, while the event stream and process controls remain independent.
async function action(callback)
{
    if (busy) return false;
    acting=true;
    elements.status.textContent='';
    render();
    try
    {
        await callback();
        state=await api('state');
        return true;
    }
    catch (error) { elements.status.textContent=error.message; return false; }
    finally { acting=false; render(); }
}

elements.theme.addEventListener('click',()=>
{
    const dark=document.documentElement.dataset.theme!=='dark';
    document.documentElement.dataset.theme=dark?'dark':'light';
    localStorage.setItem('assistant-theme',dark?'dark':'light');
    elements.theme.textContent=dark?'☼':'☾';
    elements.theme.setAttribute('aria-label',dark?'Switch to light mode':'Switch to dark mode');
});
elements['settings-open'].addEventListener('click',()=>elements['settings-dialog'].showModal());
elements['settings-close'].addEventListener('click',()=>elements['settings-dialog'].close());
elements['backend-form'].addEventListener('submit',event=>{event.preventDefault(); const backend=elements.backend.value; action(()=>api('backend',{backend}));});
elements.login.addEventListener('click',()=>action(async()=>
{
    const result=await api('login',{});
    elements['login-link'].href=result.url;
    elements['login-link'].hidden=false;
    window.open(result.url,'_blank','noopener,noreferrer');
    clearInterval(loginTimer);
    loginTimer=setInterval(async()=>
    {
        if (polling || busy) return;
        polling=true;
        try
        {
            if ((await api('login/poll',{})).complete)
            {
                clearInterval(loginTimer);
                state=await api('state');
                render();
                elements['settings-dialog'].showModal();
            }
        }
        catch (error) { clearInterval(loginTimer); elements.status.textContent=error.message; }
        finally { polling=false; }
    },2000);
}));
for (const id of ['character','commands']) elements[id].addEventListener('change',()=>
{
    const character=elements.character.value, commands_enabled=elements.commands.checked;
    if (character) action(()=>api('settings',{character,commands_enabled}));
});
elements['full-access'].addEventListener('change',async()=>
{
    const enabled=elements['full-access'].checked;
    try
    {
        if (enabled && !state.settings.commands_enabled)
            await api('settings',{character:state.settings.character,commands_enabled:true});
        state=await api('access',{enabled});
    }
    catch (error) { elements.status.textContent=error.message; }
    render();
});
// The same composer sends, queues, steers, or interrupts according to the active task.
function renderComposer()
{
    if (!state) return;
    const text=elements.prompt.value.trim(), stop=view.busy && !text, queueing=view.busy || !!state.pending;
    elements.send.textContent=stop?'■':'↑';
    elements.send.setAttribute('aria-label',stop?'Stop conversation':queueing?'Queue message':'Send message');
    elements.send.disabled=!state.settings.character || (!view.busy && state.retry_available) || (!stop && !text);
    elements['send-hint'].textContent=queueing?(view.can_steer?'Enter to queue · Ctrl+Enter to steer now':'Enter to queue · AI steering is available during generation'):'Enter to send · Shift+Enter for a new line';
    elements['steering-messages'].replaceChildren();
    for (const input of view.steering || [])
    {
        const message=document.createElement('p');
        message.textContent='You · '+input.text;
        elements['steering-messages'].append(message);
    }
    const queue=view.queue || [];
    elements['queued-messages'].hidden=!queue.length;
    elements['queued-messages'].replaceChildren();
    for (const input of queue)
    {
        const row=document.createElement('div'), text=document.createElement('span'), remove=document.createElement('button');
        text.textContent='Queued: '+input.text;
        remove.textContent='×';
        remove.dataset.remove=input.id;
        remove.setAttribute('aria-label','Remove queued message');
        row.append(text,remove);
        elements['queued-messages'].append(row);
    }
}
// Keep input editable while a previous submission is running; backend IDs protect retries.
async function submitPrompt(steer=false)
{
    const text=elements.prompt.value;
    if (!text.trim())
    {
        if (view.busy) try { await api('interrupt',{}); } catch (error) { elements.status.textContent=error.message; }
        return;
    }
    if (elements.send.disabled) return;
    if (steer && view.busy && !view.can_steer) { elements.status.textContent='No AI turn is running. Press Enter to queue this message.'; return; }
    const steering=steer && view.busy, id=steering?(steerId || (steerId=crypto.randomUUID())):crypto.randomUUID();
    elements.prompt.value='';
    renderComposer();
    try { await api(steering?'steer':'chat',{id,text}); steerId=null; }
    catch (error)
    {
        elements.status.textContent=error.message;
        if (!elements.prompt.value && (steering || !state.messages.some(message=>message.role==='user' && message.text===text))) elements.prompt.value=text;
    }
    renderComposer();
}
elements['chat-form'].addEventListener('submit',event=>{event.preventDefault(); submitPrompt();});
elements.prompt.addEventListener('keydown',event=>
{
    if (event.key==='Enter' && !event.shiftKey && !event.isComposing) { event.preventDefault(); submitPrompt(event.ctrlKey || event.metaKey); }
});
elements.prompt.addEventListener('input',()=>{steerId=null; elements.prompt.style.height='auto'; elements.prompt.style.height=Math.min(180,elements.prompt.scrollHeight)+'px'; renderComposer();});
elements['queued-messages'].addEventListener('click',async event=>
{
    const id=event.target.closest('[data-remove]')?.dataset.remove;
    if (id) try { await api('queue/'+id+'/remove',{}); } catch (error) { elements.status.textContent=error.message; }
});
for (const button of document.querySelectorAll('[data-prompt]')) button.addEventListener('click',()=>{elements.prompt.value=button.dataset.prompt; renderComposer(); elements.prompt.focus();});
for (const id of ['approve','deny']) elements[id].addEventListener('click',()=>
{
    const proposalId=state.pending.id;
    if (id==='approve') { selectedProcess=proposalId; toggleProcesses(true); }
    action(()=>api('command',{id:proposalId,approve:id==='approve'}));
});
for (const [id,path] of [['new','new'],['credits','account'],['logout','logout'],['retry','retry']])
    elements[id].addEventListener('click',()=>action(async()=>{await api(path,{}); if (id==='logout') elements['settings-dialog'].close();}));
elements.buy.addEventListener('click',()=>action(async()=>
{
    const checkout=await api('checkout',{});
    elements['checkout-link'].href=checkout.url;
    elements['checkout-link'].hidden=false;
    window.open(checkout.url,'_blank','noopener,noreferrer');
}));
elements.stop.addEventListener('click',async()=>
{
    try
    {
        await api('stop',{});
        stopped=true;
        eventController.abort();
        clearInterval(processTimer);
        clearInterval(loginTimer);
        clearInterval(elapsedTimer);
        document.body.textContent='Linux Assistant stopped. Launch the executable to start again.';
    }
    catch (error) { elements.status.textContent=error.message; }
});
let selectedProcess, processPolling=false, processRecord;
const processRows=new Map();

// Keep the floating panel usable while conversation requests are waiting on commands or AI.
function toggleProcesses(open)
{
    elements['process-panel'].hidden=!open;
    elements['process-toggle'].setAttribute('aria-expanded',String(open));
    if (open) pollProcesses();
}

// Preserve output scroll position while fresh bytes arrive, following only when already at the end.
function updateOutput(element,text)
{
    if (element.textContent===text) return;
    const follow=element.scrollHeight-element.scrollTop-element.clientHeight<24;
    element.textContent=text || '(empty)';
    if (follow) element.scrollTop=element.scrollHeight;
}

// Render metadata and stream text without interpreting command output as HTML.
function renderProcess(record)
{
    processRecord=record;
    elements['process-detail'].hidden=false;
    elements['process-metadata'].replaceChildren();
    const metadata={PID:record.pid || 'Not started',Directory:record.proposal.cwd,Started:new Date(record.started_at).toLocaleString(),
        Finished:record.finished_at?new Date(record.finished_at).toLocaleString():'Still running',
        Result:record.error || (record.finished_at?'Exit code: '+record.exit_code:'Running')};
    for (const [label,value] of Object.entries(metadata))
    {
        const term=document.createElement('dt'), detail=document.createElement('dd');
        term.textContent=label;
        detail.textContent=value;
        elements['process-metadata'].append(term,detail);
    }
    updateOutput(elements['process-command'],record.proposal.command);
    updateOutput(elements['process-arguments'],record.program+'\n'+record.arguments.map((arg,index)=>`[${index}] ${arg}`).join('\n'));
    for (const stream of ['stdout','stderr'])
    {
        const bytes=record[stream+'_bytes'];
        elements[stream+'-label'].textContent=(stream==='stdout'?'Standard output':'Standard error')+` · ${bytes.toLocaleString()} bytes`+(bytes>1048576?' · captured prefix limited to 1 MiB':'');
        updateOutput(elements['process-'+stream],record[stream]);
    }
}

// Poll a separate daemon endpoint so Stop remains available even while the chat lock is held.
async function pollProcesses()
{
    if (processPolling) return;
    processPolling=true;
    try
    {
        const records=await api('processes');
        if (selectedProcess && !records.some(record=>record.id===selectedProcess))
        {
            processRecord=null;
            elements['process-detail'].hidden=true;
        }
        const running=records.filter(record=>!record.finished_at);
        elements['process-count'].textContent=running.length+' running';
        elements['process-status'].textContent=running.length?'':'No commands are running.';
        for (const [id,row] of processRows)
            if (!running.some(record=>record.id===id)) { row.remove(); processRows.delete(id); }
        for (const record of running)
        {
            let row=processRows.get(record.id);
            if (!row)
            {
                row=document.createElement('tr');
                const command=document.createElement('td'), pid=document.createElement('td'), elapsed=document.createElement('td'), action=document.createElement('td');
                const select=document.createElement('button'), stop=document.createElement('button');
                select.className='process-select';
                select.dataset.select=record.id;
                stop.textContent='Stop';
                stop.dataset.stop=record.id;
                stop.setAttribute('aria-label','Kill this command and its child processes');
                command.append(select); action.append(stop); row.append(command,pid,elapsed,action);
                processRows.set(record.id,row);
                elements['process-list'].append(row);
            }
            row.cells[0].firstChild.textContent=record.command;
            row.cells[0].firstChild.title=record.command;
            row.cells[0].firstChild.setAttribute('aria-pressed',String(record.id===selectedProcess));
            row.cells[1].textContent=record.pid || 'Starting';
            row.cells[2].textContent=Math.floor((Date.now()-record.started_at)/1000)+'s';
        }
        elements['process-history'].replaceChildren();
        for (const record of records.filter(record=>record.finished_at))
        {
            const button=document.createElement('button');
            button.dataset.select=record.id;
            button.textContent=(record.error?'Stopped / failed':('Exit '+record.exit_code))+' · '+record.command;
            elements['process-history'].append(button);
        }
        if (!elements['process-panel'].hidden && records.some(record=>record.id===selectedProcess))
        {
            const id=selectedProcess, record=await api('processes/'+id);
            if (id===selectedProcess) renderProcess(record);
        }
    }
    catch (error) { elements['process-status'].textContent=error.message; }
    finally { processPolling=false; }
}

elements['process-toggle'].addEventListener('click',()=>toggleProcesses(elements['process-panel'].hidden));
elements['process-close'].addEventListener('click',()=>{toggleProcesses(false); elements['process-toggle'].focus();});
elements['process-panel'].addEventListener('keydown',event=>{if (event.key==='Escape') { toggleProcesses(false); elements['process-toggle'].focus(); }});
elements['process-panel'].addEventListener('click',async event=>
{
    const button=event.target.closest('button');
    if (!button) return;
    if (button.dataset.select)
    {
        selectedProcess=button.dataset.select;
        processRecord=null;
        elements['process-detail'].hidden=true;
        await pollProcesses();
    }
    if (button.dataset.stop)
    {
        button.disabled=true;
        try { await api('processes/'+button.dataset.stop+'/stop',{}); await pollProcesses(); }
        catch (error) { elements['process-status'].textContent=error.message; button.disabled=false; }
    }
});
for (const stream of ['stdout','stderr']) elements['download-'+stream].addEventListener('click',()=>
{
    if (!processRecord) return;
    const url=URL.createObjectURL(new Blob([processRecord[stream]],{type:'text/plain;charset=utf-8'})), link=document.createElement('a');
    link.href=url;
    link.download=processRecord.id+'-'+stream+'.txt';
    link.click();
    setTimeout(()=>URL.revokeObjectURL(url),1000);
});


const processTimer=setInterval(()=>{if (state?.authenticated) pollProcesses();},1000);
const elapsedTimer=setInterval(()=>{elements.elapsed.textContent=startedAt?Math.floor((Date.now()-startedAt)/1000)+'s':'';},1000);
watch();
api('state').then(async current=>
{
    state=current;
    render();
    if (state.authenticated && !state.settings.character) elements['settings-dialog'].showModal();
    if (state.authenticated && !view.busy)
    {
        try { state=await api('account',{}); render(); } catch (error) { elements.status.textContent=error.message; }
    }
}).catch(error=>{elements.status.textContent=error.message;});
