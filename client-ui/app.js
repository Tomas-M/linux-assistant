'use strict';
const elements=Object.fromEntries([...document.querySelectorAll('[id]')].map(element=>[element.id,element]));
let steerId, editingId, startingId, queueAction=false, loginBusy=false, buying=false, resendAt=0, state, view={busy:false,steps:[],runs:[],commands:[],connections:{}}, busy=false, acting=false, startedAt, renderFrame, followOutput=true;
const pendingIds=new Set(), messageRows=[], completionRows=[];
const progressViews=new WeakMap(), steeringRows=new Map();
let queueKey='', steeringKey='', themePreference, lastTaskError;
const panelWidths=new Map();
let socialLogin, enabledProviders=[], emailEnabled=false, refreshingBalance=false, providersReady=false;
let memoryDocument, memoryRequest=0, memoryLoading=false, memorySaving=false;
const providerNames={google:'Google',microsoft:'Microsoft',github:'GitHub'};

// All icons reference the embedded SVG sprite; no fonts or external assets are needed.
function setIcon(button,name)
{
    if (button.dataset.icon===name) return;
    button.dataset.icon=name;
    button.innerHTML='<svg aria-hidden="true"><use href="#icon-'+name+'"/></svg>';
}

// Render a new notification once; live snapshots cannot reopen a dismissed task error.
function showError(message)
{
    delete elements.status.dataset.taskError;
    elements['status-message'].textContent=message;
    elements.status.hidden=!message;
}

elements['status-dismiss'].addEventListener('click',()=>{ elements.status.hidden=true; });
elements['balance-dismiss'].addEventListener('click',()=>
{
    elements['balance-note'].dataset.dismissed='true';
    elements['balance-note'].hidden=true;
});

// Return conversation actions to the prompt without interrupting a dialog or another text field.
function focusPrompt()
{
    if (elements.prompt.disabled || document.querySelector('dialog[open]')) return;
    const active=document.activeElement;
    if (active.closest('#memories-panel') && !elements['memories-panel'].hidden) return;
    if (active!==elements.prompt && !active.disabled && active.matches('textarea,input:not([type=checkbox]):not([type=radio]),[contenteditable]')) return;
    elements.prompt.focus({preventScroll:true});
}

// Match Codex's latest-summary-line status title without treating Markdown punctuation as protocol events.
function progressTitle(parts)
{
    for (const part of [...parts].reverse())
    {
        if (part.kind!=='reasoning') continue;
        for (let line of part.text.split(/\r?\n/u).reverse())
        {
            line=line.trim();
            if (!line || line.startsWith('<!--')) continue;
            line=line.replace(/^#+\s*/u,'');
            if (line.startsWith('**'))
            {
                const end=line.indexOf('**',2);
                if (end<0) continue;
                line=line.slice(2,end)+line.slice(end+2);
            }
            return Array.from(line).slice(0,160).join('');
        }
    }
    return '';
}

// The server supplies the default; a user's explicit preference is scoped to this backend.
function applyTheme(theme)
{
    if (document.documentElement.dataset.theme!==theme) document.documentElement.dataset.theme=theme;
    setIcon(elements.theme,theme==='dark'?'sun':'moon');
    elements.theme.setAttribute('aria-label',theme==='dark'?'Switch to light mode':'Switch to dark mode');
}

// Keep every operation on the authenticated loopback origin.
async function api(path,body,signal)
{
    const response=await fetch('/api/'+path,{method:body===undefined?'GET':'POST',headers:{'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal});
    if (response.status===413) throw new Error('This request exceeds the HTTP transport size limit (64 MiB). Nothing was sent to the AI.');
    const value=await response.json();
    if (!response.ok) throw new Error(value.error || 'Request failed');
    return value;
}

// Replies use safe Markdown; command output and user input remain plain text.
function render()
{
    if (!state) return;
    const finished=busy && !view.busy && !acting;
    busy=view.busy || acting;
    if (finished && !elements['memories-panel'].hidden && !memoryDirty()) loadMemories();
    const initializing=document.body.classList.contains('initializing');
    const configured=!!view.brand,ready=configured && providersReady;
    document.body.classList.toggle('initializing',!ready);
    elements.startup.hidden=ready || !view.backend_error;
    if (!ready) { renderConnection(); return; }
    if (initializing && emailEnabled && !state.authenticated) queueMicrotask(()=>elements.email.focus({preventScroll:true}));
    const brand=view.brand,images=brand.images;
    document.title=brand.name;
    elements['brand-name'].textContent=brand.name;
    elements.brand.setAttribute('aria-label',brand.name+' home');
    for (const element of document.querySelectorAll('[data-brand-name]')) if (element.textContent!==brand.name) element.textContent=brand.name;
    applyTheme(themePreference || brand.default_theme);
    let logo=null;
    if (images['logo-full']) logo='logo-full';
    else if (images['logo-image']) logo='logo-image';
    elements['brand-logo'].hidden=!logo;
    elements['brand-mark'].hidden=!!logo;
    elements['brand-name'].hidden=logo==='logo-full';
    elements['brand-logo'].classList.toggle('brand-mark-image',logo==='logo-image');
    for (const [element,key,attribute] of [[elements['brand-logo'],logo,'src'],[elements['assistant-image'],'assistant-image','src'],[elements.favicon,'favicon','href']])
    {
        const hash=key && images[key];
        element.hidden=!hash;
        if (element.dataset.hash===hash) continue;
        if (hash) { element.setAttribute(attribute,'/branding/'+key+'?v='+hash); element.dataset.hash=hash; }
        else { if (key==='favicon') element.setAttribute(attribute,'data:,'); else element.removeAttribute(attribute); delete element.dataset.hash; }
    }
    elements['brand-logo'].alt=brand.name;
    document.body.classList.toggle('signed-out',!state.authenticated);
    const emptyChat=state.authenticated && !state.messages.length && !view.busy;
    document.body.classList.toggle('empty-chat',emptyChat);
    elements.setup.hidden=state.authenticated;
    elements['composer-wrap'].hidden=!state.authenticated;
    elements['credit-balance'].hidden=!state.authenticated;
    elements['process-toggle'].hidden=!state.authenticated;
    elements['memories-toggle'].hidden=!state.authenticated;
    elements.welcome.hidden=!emptyChat;
    const actions=brand.quick_actions,actionsKey=JSON.stringify(actions);
    if (elements['quick-actions'].dataset.actions!==actionsKey)
    {
        elements['quick-actions'].dataset.actions=actionsKey;
        elements['quick-actions'].hidden=!actions.length;
        elements['quick-actions'].replaceChildren(...actions.map((action,index)=>
        {
            const button=document.createElement('button');
            button.type='button'; button.dataset.quickAction=index; button.textContent=action.label;
            return button;
        }));
    }
    if (!state.authenticated)
    {
        toggleProcesses(false); toggleMemories(false);
        for (const dialog of document.querySelectorAll('dialog[open]')) dialog.close('cancel');
        memoryRequest++; memoryDocument=undefined; memoryLoading=false; memorySaving=false;
        elements['memories-text'].value=''; elements['memories-status'].textContent=''; memoryControls();
    }
    // Command IDs are scoped to this chat, so a new empty chat immediately clears the task display.
    if (!view.commands.length)
    {
        selectProcess(null);
        historyKey='';
        elements['process-list'].replaceChildren(); processRows.clear();
        elements['process-list'].closest('.task-table-wrap').hidden=true;
        elements['process-history'].replaceChildren();
        elements['process-count'].hidden=true;
        elements['tasks-running'].hidden=true;
        elements['process-toggle'].setAttribute('aria-label','Tasks');
        elements['process-toggle'].title='Task manager';
        elements['process-status'].textContent='No commands in this chat yet.';
        elements['history-heading'].textContent='Completed commands · 0';
    }
    elements['full-access'].checked=state.full_access;
    elements['full-access'].disabled=!state.authenticated;
    elements['permission-note'].textContent=state.full_access?'Commands run automatically':state.settings.commands_enabled?'You approve every command':'Local commands are off';
    elements.platform.textContent=(state.platform==='windows'?'Windows · PowerShell':'Linux · sh')+' · Local workspace';
    elements.account.textContent=state.account?.email || 'Sign in to your assistant account';
    elements['settings-balance'].textContent=state.account?state.account.credits:'Loading…';
    if (elements.purchases.dataset.email!==state.account?.email)
    {
        elements.purchases.replaceChildren();
        elements.purchases.dataset.email=state.account?.email || '';
        elements['purchase-history'].hidden=true;
    }
    elements['credit-balance'].textContent=state.account?state.account.credits+' credits':'Credits…';
    const depleted=state.account && state.account.credits<=0;
    for (const id of ['credit-balance','settings-balance']) elements[id].classList.toggle('depleted',!!depleted);
    if (!depleted) delete elements['balance-note'].dataset.dismissed;
    elements['balance-note'].hidden=!depleted || elements['balance-note'].dataset.dismissed==='true';
    elements.credits.disabled=refreshingBalance || !state.authenticated;
    elements.logout.disabled=busy || !state.authenticated;
    elements.logout.title=busy?'Stop the running task before signing out':'';
    for (const id of ['new','retry'])
        elements[id].disabled=busy || !state.authenticated;
    elements['buy-inline'].disabled=buying || !state.account;
    for (const button of elements['credit-packs'].querySelectorAll('button')) button.disabled=buying || !state.account || elements['credit-packs'].dataset.development==='true';
    renderConnection();
    renderComposer();
    renderLogin();
    elements.retry.hidden=!state.retry_available || busy;
    renderQuestions();
    const steps=(view.steps || []).filter(step=>step.after<state.messages.length);
    const progressByMessage=new Map();
    for (const step of steps)
    {
        if (!progressByMessage.has(step.after)) progressByMessage.set(step.after,[]);
        progressByMessage.get(step.after).push(step);
    }
    // Reuse unchanged messages so expanded output, text selection and keyboard focus survive new replies.
    while (messageRows.length>state.messages.length) messageRows.pop().element.remove();
    state.messages.forEach((message,index)=>
    {
        const previous=messageRows[index];
        const progress=progressByMessage.get(index) || [], key=JSON.stringify([progress,message.attachments]);
        if (previous?.text===message.text && previous.role===message.role && previous.progress===key) return;
        const group=document.createElement('div');
        if (previous) previous.element.replaceWith(group);
        else elements.messages.append(group);
        messageRows[index]={element:group,text:message.text,role:message.role,progress:key,thinking:[]};
        const progressContainer=document.createElement('div');
        renderProgress(progressContainer,progress.flatMap(step=>step.summaries));
        if (progressContainer.hasChildNodes()) { group.append(progressContainer); messageRows[index].thinking=[progressContainer]; }
        const article=document.createElement('article');
        article.className=message.role;
        const text=document.createElement('div');
        text.className='answer';
        if (message.role==='assistant') renderAnswer(text,message.text);
        else text.textContent=message.text;
        article.append(text);
        appendPromptAttachments(group,message.attachments);
        article.hidden=message.role==='user' && !message.text.trim();
        group.append(article);
        if (message.role==='user')
        {
            group.className='user-message';
            const actions=document.createElement('div'), copy=document.createElement('button');
            actions.className='prompt-actions';
            copy.className='icon-button copy-prompt';
            copy.dataset.copyPrompt=index;
            copy.title='Copy message';
            copy.setAttribute('aria-label','Copy message');
            setIcon(copy,'copy');
            actions.append(copy);
            group.append(actions);
        }
    });
    renderCompactions();
    renderCompletions();
    const activity=view.activity, parts=activity?.progress?.summaries || [];
    const steers=(view.steering || []).filter(input=>input.message_index===undefined);
    elements.activity.hidden=!activity || !view.busy;
    elements['activity-label'].classList.toggle('working',view.busy);
    if (activity)
    {
        elements['activity-label'].textContent=activity.label;
        elements['activity-icon'].toggleAttribute('hidden',activity.progress!==null);
        elements['activity-label'].parentElement.hidden=view.interactions.length>0;
        renderProgress(elements.summaries,parts,view.busy && !view.interactions.length,steers);
        renderImageGallery(elements['agent-images'],Object.values(view.tool_results).map(result=>result.image).filter(image=>image?.run===view.runs.at(-1)?.started_at));
        renderAnswer(elements['stream-text'],activity.progress?.text || '');
        elements['stream-text'].classList.toggle('streaming',view.busy);
    }
    const errorKey=view.error?JSON.stringify([view.runs.at(-1)?.started_at,view.error]):null;
    if (errorKey!==lastTaskError)
    {
        lastTaskError=errorKey;
        if (view.error) { showError(view.error); elements.status.dataset.taskError='true'; }
        else if (elements.status.dataset.taskError) showError('');
    }
    renderScrollButton();
}

// Keep each nested action open and stable while its result or verified diff arrives.
function updateAction(row,key,icon,label,output,process,change)
{
    let item=row.actions.get(key);
    if (!item)
    {
        const element=document.createElement('details'), summary=document.createElement('summary'), pre=document.createElement('pre');
        element.className='action-item';
        summary.innerHTML='<svg aria-hidden="true"><use href="#icon-'+icon+'"/></svg><span></span><svg class="disclosure-arrow" aria-hidden="true"><use href="#icon-chevron"/></svg>';
        element.append(summary,pre); enableDisclosureAnimation(element);
        item={element,summary,pre}; row.actions.set(key,item); row.body.append(element);
    }
    const position=row.body.children[row.actionPosition++];
    if (position!==item.element) row.body.insertBefore(item.element,position || null);
    if (item.summary.querySelector('span').textContent!==label) item.summary.querySelector('span').textContent=label;
    if (item.summary.title!==label) item.summary.title=label;
    const use=item.summary.querySelector('use');
    if (use.getAttribute('href')!=='#icon-'+icon) use.setAttribute('href','#icon-'+icon);
    if (process && !item.button)
    {
        item.button=document.createElement('button'); item.button.className='quiet'; item.button.textContent='Open in task manager'; item.element.append(item.button);
    }
    if (item.button) { item.button.hidden=!process; if (process) item.button.dataset.process=process; }
    if (item.pre.textContent!==output) item.pre.textContent=output;
    const counts=change && change.added!==null?change.added+':'+change.removed:null;
    if (item.counts!==counts)
    {
        item.counts=counts;
        if (item.count) { item.count.remove(); item.count=null; }
        if (counts===null) return;
        item.count=document.createElement('small'); item.count.className='diff-count';
        const added=document.createElement('span'), removed=document.createElement('span');
        added.className='added'; added.textContent='+'+change.added;
        removed.className='removed'; removed.textContent=' −'+change.removed;
        item.count.append(added,removed); item.summary.insertBefore(item.count,item.summary.lastChild);
    }
}

// Completed compactions remain visible even when generation is stopped before an answer arrives.
const compactionRows=new Map();
function renderCompactions()
{
    const present=new Set();
    for (const step of view.steps)
    {
        for (const id of step.compacted || [])
        {
            present.add(id);
            let element=compactionRows.get(id);
            if (!element)
            {
                element=document.createElement('div'); element.className='compaction-note';
                element.innerHTML='<svg aria-hidden="true"><use href="#icon-compact"/></svg><span>Context automatically compacted</span>';
                compactionRows.set(id,element);
            }
            const anchor=messageRows[step.after-1]?.element;
            if (anchor) { if (anchor.nextSibling!==element) anchor.after(element); }
            else if (elements.messages.firstChild!==element) elements.messages.prepend(element);
        }
    }
    for (const [id,element] of compactionRows) if (!present.has(id)) { element.remove(); compactionRows.delete(id); }
}

// Fold completed work into a stable disclosure while keeping prompts and the final answer visible.
function renderCompletions()
{
    const runs=view.runs.filter(run=>run.finished_at), anchors=new Map(), compactions=new Map();
    for (const step of view.steps)
    {
        if (!compactions.has(step.after)) compactions.set(step.after,[]);
        for (const id of step.compacted || []) compactions.get(step.after).push(compactionRows.get(id));
    }
    while (completionRows.length>runs.length) completionRows.pop().element.remove();
    runs.forEach((run,index)=>
    {
        const key=JSON.stringify(run);
        let row=completionRows[index];
        if (row?.key!==key)
        {
            const element=document.createElement('section'), work=document.createElement('details'), summary=document.createElement('summary');
            const body=document.createElement('div'), answer=document.createElement('div');
            element.className='completion'; work.className='work-summary'; body.className='work-body';
            const seconds=Math.max(0,Math.round((run.finished_at-run.started_at)/1000));
            const duration=(seconds>=60?Math.floor(seconds/60)+'m ':'')+seconds%60+'s';
            summary.innerHTML='<span></span><svg aria-hidden="true"><use href="#icon-chevron"/></svg>';
            summary.firstChild.textContent='Worked for '+duration;
            const images=document.createElement('div'); images.className='image-gallery';
            work.append(summary,body); enableDisclosureAnimation(work); element.append(work,images,answer);
            let notes;
            if (run.summaries.some(part=>part.text.trim()))
            {
                notes=document.createElement('div'); notes.className='summaries';
                renderProgress(notes,run.summaries);
            }
            if (run.partial)
            {
                const partial=document.createElement('div');
                partial.className='answer partial-answer'; renderAnswer(partial,run.partial); answer.append(partial);
            }
            const footer=document.createElement('div'), copy=document.createElement('button'), time=document.createElement('time'), status=document.createElement('span');
            footer.className='completion-footer'; copy.className='icon-button copy-answer'; copy.dataset.copy=index;
            copy.title='Copy response'; copy.setAttribute('aria-label','Copy response');
            copy.disabled=!run.partial && !state.messages.slice(run.start,run.end).some(message=>message.role==='assistant' && message.text);
            setIcon(copy,'copy');
            const date=new Date(run.finished_at);
            time.dateTime=date.toISOString(); time.textContent=date.toLocaleTimeString([],{hour:'2-digit',minute:'2-digit'}); time.title=date.toLocaleString();
            status.textContent=run.error || 'Completed'; status.title='Worked for '+duration;
            const credits=document.createElement('span');
            credits.className='completion-cost';
            credits.textContent=run.credits==null?'Credits unavailable':run.credits+' '+(run.credits===1?'credit':'credits');
            footer.append(copy,time,status,credits); element.append(footer);
            if (row) row.element.replaceWith(element);
            completionRows[index]=row={element,work,body,answer,images,notes,key};
        }
        renderImageGallery(row.images,Object.values(view.tool_results).map(result=>result.image).filter(image=>image?.run===run.started_at));
        const history=[], finalIndex=!run.partial && state.messages[run.end-1]?.role==='assistant'?run.end-1:-1;
        let anchor=anchors.get(run.start) || messageRows[run.start-1]?.element;
        anchor=anchor?.closest('.completion') || anchor;
        for (let messageIndex=run.start;messageIndex<run.end;messageIndex++)
        {
            history.push(...(compactions.get(messageIndex) || []));
            const message=messageRows[messageIndex];
            if (state.messages[messageIndex].role==='user' || messageIndex===finalIndex)
            {
                history.push(...(message.thinking || []));
                if (state.messages[messageIndex].role==='user') anchor=message.element;
            }
            else history.push(message.element);
        }
        const items=[...new Set(history)];
        if (row.notes) items.push(row.notes);
        items.forEach((element,position)=>
        {
            if (row.body.children[position]!==element) row.body.insertBefore(element,row.body.children[position] || null);
        });
        const present=new Set(items);
        for (const child of [...row.body.children]) if (!present.has(child)) child.remove();
        row.work.hidden=!items.length;
        if (finalIndex>=0 && messageRows[finalIndex].element.parentElement!==row.answer) row.answer.append(messageRows[finalIndex].element);
        if (anchor && row.element.previousElementSibling!==anchor) anchor.after(row.element);
        else if (!anchor && elements.messages.firstElementChild!==row.element) elements.messages.prepend(row.element);
        anchors.set(run.end,row.element);
    });
}

// Copy the final response, or its interrupted preview, without commands or progress chatter.
elements.workspace.addEventListener('click',async event=>
{
    const copy=event.target.closest('[data-copy],[data-copy-prompt],[data-copy-steering],[data-copy-file-path]'), process=event.target.closest('[data-process]');
    if (copy)
    {
        const prompt=copy.hasAttribute('data-copy-prompt') || copy.hasAttribute('data-copy-steering');
        let text,label=prompt?'Copy message':'Copy response';
        if (copy.hasAttribute('data-copy-file-path')) { text=copy.dataset.copyFilePath; label='Copy file path'; }
        else if (copy.hasAttribute('data-copy-steering')) text=view.steering.find(input=>input.id===copy.dataset.copySteering).text;
        else if (prompt) text=state.messages[Number(copy.dataset.copyPrompt)].text;
        else
        {
            const run=view.runs.filter(run=>run.finished_at)[Number(copy.dataset.copy)];
            text=run.partial || state.messages.slice(run.start,run.end).findLast(message=>message.role==='assistant')?.text || '';
        }
        try
        {
            await navigator.clipboard.writeText(text);
            setIcon(copy,'check');
            copy.title='Copied';
            copy.setAttribute('aria-label','Copied');
            setTimeout(()=>{setIcon(copy,'copy');copy.title=label;copy.setAttribute('aria-label',label);},1600);
        }
        catch (error) { showError('Could not copy: '+error.message); }
    }
    if (process)
    {
        selectProcess(process.dataset.process);
        toggleProcesses(true);
    }
});

// Show a way back to the latest output whenever the reader is away from the end.
function renderScrollButton()
{
    const button=elements['scroll-bottom'];
    button.dataset.icon=view.busy && !button.matches(':hover,:focus-visible')?'ellipsis':'send';
    button.hidden=!state?.authenticated || document.body.classList.contains('empty-chat') || document.documentElement.scrollHeight-window.scrollY-window.innerHeight<=4;
}

// Explicit jumps and submitted prompts resume following subsequent streamed updates.
function scrollToEnd()
{
    followOutput=true;
    window.scrollTo({top:document.documentElement.scrollHeight,behavior:'instant'});
    renderScrollButton();
}

window.addEventListener('scroll',()=>
{
    followOutput=document.documentElement.scrollHeight-window.scrollY-window.innerHeight<=4;
    renderScrollButton();
},{passive:true});
elements['scroll-bottom'].addEventListener('click',scrollToEnd);
for (const event of ['pointerenter','pointerleave','focus','blur']) elements['scroll-bottom'].addEventListener(event,renderScrollButton);
window.addEventListener('resize',()=>{if (followOutput) scrollToEnd(); else renderScrollButton();});

// Shared snapshots restore an active conversation after refresh without posting another request.
async function watch()
{
    while (true)
    {
        try
        {
            const response=await fetch('/api/events');
            if (!response.ok) throw new Error('The local assistant rejected the connection.');
            if (elements.status.dataset.connection) delete elements.status.dataset.connection;
            const reader=response.body.getReader(), decoder=new TextDecoder();
            let buffer='';
            while (true)
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
                    const next=JSON.parse(data);
                    startedAt=next.busy?next.runs.at(-1)?.started_at:null;
                    view=next;
                    state=view.state;
                    for (const id of pendingIds)
                        if (!(view.queue || []).some(input=>input.id===id && input.editing)) pendingIds.delete(id);
                    if (!renderFrame) renderFrame=requestAnimationFrame(()=>
                    {
                        renderFrame=null;
                        render();
                        if (followOutput) scrollToEnd();
                    });
                }

            }
        }
        catch (error)
        {
            elements.status.dataset.connection='true';
            renderConnection();
            await new Promise(resolve=>setTimeout(resolve,1500));
        }
    }
}

// Mutations stay serialized, while the event stream and process controls remain independent.
async function action(callback)
{
    if (busy) return false;
    acting=true;
    showError('');
    render(); focusPrompt();
    try
    {
        await callback();
        state=await api('state');
        return true;
    }
    catch (error) { showError(error.message); return false; }
    finally { acting=false; render(); }
}

elements.theme.addEventListener('click',()=>
{
    const dark=document.documentElement.dataset.theme!=='dark';
    document.documentElement.dataset.theme=dark?'dark':'light';
    themePreference=dark?'dark':'light';
    setIcon(elements.theme,dark?'sun':'moon');
    elements.theme.setAttribute('aria-label',dark?'Switch to light mode':'Switch to dark mode');
});
// Sign out through the shared client session and return directly to the email form.
elements.logout.addEventListener('click',async()=>
{
    elements.logout.disabled=true;
    try
    {
        await api('logout',{}); state=await api('state');
        elements.prompt.value=''; draftAttachments=[]; editingId=null; steerId=null; startingId=null; pendingIds.clear(); renderAttachments();
        render(); if (emailEnabled) elements.email.focus({preventScroll:true});
    }
    catch (error) { elements['settings-status'].textContent=error.message; }
    finally { elements.logout.disabled=busy || !state.authenticated; }
});
// A failed startup can be retried explicitly; a successful startup never reloads settings.
elements['configuration-retry'].addEventListener('click',async()=>
{
    elements['configuration-retry'].disabled=true;
    try { const result=await api('configuration/retry',{}); if (result.providers) { enabledProviders=result.providers.providers; emailEnabled=result.providers.email_enabled; renderLogin(); render(); } }
    catch (error) { elements['startup-status'].textContent=error.message; }
    finally { elements['configuration-retry'].disabled=false; }
});

// Show only the latest three purchases, and keep empty history out of Settings.
async function loadPurchases()
{
    const email=state.account?.email;
    if (!email) return;
    try
    {
        const result=await api('purchases');
        if (state.account?.email!==email) return;
        const rows=document.createDocumentFragment();
        for (const purchase of result.purchases.slice(0,3))
        {
            const row=document.createElement('li'),credits=document.createElement('strong'),time=document.createElement('time');
            credits.textContent='+'+purchase.credits+' credits';
            time.dateTime=new Date(purchase.paid_at*1000).toISOString();
            time.textContent=new Date(purchase.paid_at*1000).toLocaleString();
            row.append(credits,time); rows.append(row);
        }
        elements.purchases.replaceChildren(rows);
        elements['purchase-history'].hidden=!result.purchases.length;
    }
    catch (error) { if (state.account?.email===email) elements['settings-status'].textContent=error.message; }
}

// Fetch purchase choices whenever Settings opens so administrator changes appear without restarting the client.
async function loadCreditPacks()
{
    const email=state.account?.email;
    if (!email) return;
    if (!elements['credit-packs'].hasChildNodes()) elements['credit-packs-status'].textContent='Loading credit packs…';
    try
    {
        const result=await api('credit-packs');
        if (state.account?.email!==email) return;
        elements['credit-packs'].dataset.development=String(result.development);
        const packs=document.createDocumentFragment();
        for (const [index,pack] of result.packs.entries())
        {
            const button=document.createElement('button'),icon=document.createElement('span'),amount=document.createElement('span'),credits=document.createElement('strong'),label=document.createElement('span'),price=document.createElement('span'),currency=document.createElement('span');
            button.type='button'; button.className='credit-pack'; button.dataset.priceId=pack.price_id;
            icon.className='credit-pack-icon'; setIcon(icon,['coins','layer-group','gem','rocket'][index%4]);
            credits.textContent=pack.credits; label.textContent='credits';
            amount.className='credit-pack-amount'; amount.append(credits,label);
            price.className='credit-pack-price'; price.textContent=new Intl.NumberFormat(undefined,{style:'currency',currency:'USD'}).format(Number(pack.usd));
            currency.textContent=' USD'; price.append(currency);
            button.setAttribute('aria-label','Buy '+pack.credits+' credits for '+price.textContent);
            button.append(icon,amount,price); packs.append(button);
        }
        elements['credit-packs'].replaceChildren(packs);
        elements['credit-packs-status'].textContent=result.development?'Purchases are disabled in development mode.':result.packs.length?'Choose a pack to open checkout.':'No credit packs are currently available.';
        render();
    }
    catch (error) { if (state.account?.email===email) elements['credit-packs-status'].textContent=error.message; }
}

// Fetch the credit overview and recent purchases whenever Settings opens.
function openSettings()
{
    elements['settings-status'].textContent='';
    elements['settings-dialog'].showModal();
    loadCreditPacks();
    loadPurchases();
}
elements['settings-close'].addEventListener('click',()=>elements['settings-dialog'].close());
elements['credit-balance'].addEventListener('click',openSettings);
elements.credits.addEventListener('click',()=>
{
    elements.credits.querySelector('svg').animate([{transform:'rotate(0deg)'},{transform:'rotate(360deg)'}],{duration:600,easing:'ease-in-out'});
    refreshBalance();
});

// Update account figures in place; a balance refresh never clears or reloads the credit-pack cards.
async function refreshBalance()
{
    if (refreshingBalance || !state?.authenticated) return false;
    const email=state.account?.email;
    refreshingBalance=true; elements.credits.disabled=true;
    elements['settings-status'].textContent='';
    try
    {
        const current=await api('account',{});
        if (email && state.account?.email!==email) return;
        state=current; render();
        await loadPurchases();
        return true;
    }
    catch (error) { elements['settings-status'].textContent=error.message; return false; }
    finally { refreshingBalance=false; elements.credits.disabled=!state.authenticated; }
}

// The popup may lose its opener across Stripe redirects; confirmed purchases still update this window.
async function watchPurchase(id,email)
{
    for (let attempt=0;attempt<450 && state.account?.email===email;attempt++)
    {
        await new Promise(resolve=>setTimeout(resolve,2000));
        if (state.account?.email!==email) return;
        try
        {
            const purchase=await api('purchases/'+id);
            if (purchase.paid && await refreshBalance()) return;
        }
        catch { /* A temporary connection failure does not lose the payment confirmation. */ }
    }
}
const codeInputs=[...elements['code-digits'].querySelectorAll('input')];

// Keep the sign-in form local, including resend timing and development-only delivery hints.
function renderLogin()
{
    elements['code-email'].textContent=elements.email.value.trim();
    const remaining=Math.max(0,Math.ceil((resendAt-Date.now())/1000));
    const waiting=!!socialLogin && !socialLogin.verification;
    // Keep the form visibly locked after the popup closes until polling confirms completion or cancellation.
    elements['setup'].classList.toggle('finishing-sign-in',waiting && !!socialLogin.popup?.closed);
    const emailAllowed=emailEnabled || !!socialLogin?.verification;
    elements['email-sign-in'].hidden=!emailAllowed;
    elements['auth-divider'].hidden=!emailAllowed;
    for (const id of ['email','login','verify','change-email']) elements[id].disabled=loginBusy || waiting || !emailAllowed;
    elements['code-digits'].disabled=loginBusy;
    elements.resend.disabled=loginBusy || remaining>0;
    elements.resend.textContent=remaining?'Resend in '+remaining+'s':'Resend code';
    elements['social-providers'].hidden=!enabledProviders.length || !!socialLogin?.verification;
    for (const button of document.querySelectorAll('[data-provider]'))
    {
        button.hidden=!enabledProviders.includes(button.dataset.provider);
        button.disabled=loginBusy || !!socialLogin;
    }
}

// Email and social sign-in share one success transition, including clearing old session errors.
function signedIn(current)
{
    socialLogin?.popup?.close();
    socialLogin=undefined;
    state=current;
    view.error=null;
    showError('');
    delete elements.status.dataset.taskError;
    elements['auth-status'].textContent='';
    elements['code-form'].hidden=true;
    elements['auth-methods'].hidden=false;
    for (const input of codeInputs) input.value='';
    renderLogin(); render();
    elements.prompt.focus();
}

// Poll through the native client; neither application sessions nor device secrets enter the popup.
async function pollSocial()
{
    const attempt=socialLogin;
    if (!attempt?.id || attempt.verification || loginBusy) return;
    if (state?.authenticated) { signedIn(state); return; }
    try
    {
        const result=await api('login/poll',{id:attempt.id});
        if (socialLogin!==attempt) return;
        if (result.state) { signedIn(result.state); return; }
        if (result.email_verification)
        {
            attempt.verification=true;
            attempt.popup?.close();
            elements.email.value=result.email_verification.email || '';
            elements['auth-status'].textContent='Confirm your email once to finish linking '+providerNames[attempt.provider]+'.';
            renderLogin(); elements.email.focus();
            return;
        }
        // Check completion first because successful sign-in also closes the popup.
        if (attempt.popup?.closed)
        {
            await cancelSocial();
            return;
        }
    }
    catch (error)
    {
        if (socialLogin!==attempt) return;
        attempt.popup?.close();
        try { await api('login/cancel',{id:attempt.id}); if (socialLogin===attempt) socialLogin=undefined; }
        catch { if (socialLogin===attempt) setTimeout(pollSocial,2000); }
        elements['auth-status'].textContent=error.message;
        renderLogin();
        return;
    }
    if (socialLogin===attempt) setTimeout(pollSocial,2000);
}

document.querySelectorAll('[data-provider]').forEach(button=>button.addEventListener('click',async()=>
{
    if (loginBusy || socialLogin) return;
    const popup=window.open('about:blank','_blank','popup,width=520,height=720');
    if (!popup) { elements['auth-status'].textContent='Allow popups for this local page, then try signing in again.'; return; }
    popup.opener=null;
    const attempt={provider:button.dataset.provider,popup};
    socialLogin=attempt;
    loginBusy=true;
    elements['auth-status'].textContent='';
    renderLogin();
    try
    {
        const result=await api('login/social',{provider:attempt.provider});
        Object.assign(attempt,{id:result.id,expires_at:result.expires_at});
        popup.location=result.url;
    }
    catch (error) { popup.close(); socialLogin=undefined; elements['auth-status'].textContent=error.message; }
    finally { loginBusy=false; renderLogin(); }
    pollSocial();
}));
// Closing a provider popup cancels its pending sign-in without adding controls to the form.
async function cancelSocial()
{
    if (loginBusy || !socialLogin?.id) return;
    loginBusy=true; renderLogin();
    try
    {
        await api('login/cancel',{id:socialLogin.id});
        socialLogin.popup?.close(); socialLogin=undefined;
        elements['auth-methods'].hidden=false; elements['code-form'].hidden=true;
        elements['auth-status'].textContent='';
        elements.email.focus();
    }
    catch (error) { elements['auth-status'].textContent=error.message; setTimeout(pollSocial,2000); }
    finally { loginBusy=false; renderLogin(); }
}

// Both the initial request and resend use the same email and never navigate away.
async function requestCode()
{
    if (loginBusy || !elements['email-form'].reportValidity()) return;
    elements.login.setAttribute('aria-busy','true');
    loginBusy=true;
    elements['auth-status'].textContent='';
    renderLogin();
    try
    {
        const result=await api('login',{email:elements.email.value.trim()});
        resendAt=Date.now()+60000;
        elements['auth-methods'].hidden=true;
        elements['code-form'].hidden=false;
        elements['development-code'].hidden=!result.development_code;
        elements['development-code'].textContent=result.development_code?'Development code: '+result.development_code:'';
        for (const input of codeInputs) input.value='';
    }
    catch (error) { elements['auth-status'].textContent=error.message; }
    finally { elements.login.removeAttribute('aria-busy'); loginBusy=false; renderLogin(); if (!elements['code-form'].hidden) codeInputs[0].focus(); }
}
elements['email-form'].addEventListener('submit',event=>{event.preventDefault(); requestCode();});
elements.resend.addEventListener('click',requestCode);
elements['change-email'].addEventListener('click',()=>
{
    elements['auth-methods'].hidden=false;
    elements['code-form'].hidden=true;
    elements['auth-status'].textContent='';
    elements.email.focus();
});
elements['code-form'].addEventListener('submit',async event=>
{
    event.preventDefault();
    if (loginBusy) return;
    const code=codeInputs.map(input=>input.value).join('').trim();
    if (!/^[0-9]{6}$/u.test(code)) { elements['auth-status'].textContent='Enter all six digits.'; return; }
    loginBusy=true;
    elements['auth-status'].textContent='';
    renderLogin();
    try
    {
        signedIn(await api('login/verify',{code}));
    }
    catch (error) { elements['auth-status'].textContent=error.message; }
    finally
    {
        loginBusy=false; renderLogin();
        // Disabling the fieldset during verification drops focus; restore it after a failed attempt.
        if (!elements['code-form'].hidden) { codeInputs.at(-1).focus(); codeInputs.at(-1).select(); }
    }
});

// Accept a full pasted/autofilled code in any cell, or advance one digit at a time.
function fillCode(value,index)
{
    const digits=value.replace(/\s/gu,'');
    if (!/^[0-9]{0,6}$/u.test(digits)) { elements['auth-status'].textContent='Use the six digits from your email.'; codeInputs[index].value=''; return; }
    elements['auth-status'].textContent='';
    if (digits.length===6) index=0;
    if (!digits.length) { codeInputs[index].value=''; return; }
    for (let offset=0; offset<digits.length && index+offset<6; offset++) codeInputs[index+offset].value=digits[offset];
    codeInputs[Math.min(5,index+digits.length)].focus();
    if (codeInputs.every(input=>/^[0-9]$/u.test(input.value))) elements['code-form'].requestSubmit();
}
codeInputs.forEach((input,index)=>
{
    input.addEventListener('focus',()=>input.select());
    input.addEventListener('input',()=>fillCode(input.value,index));
    input.addEventListener('paste',event=>{event.preventDefault(); fillCode(event.clipboardData.getData('text').trim(),index);});
    input.addEventListener('keydown',event=>
    {
        if (event.key==='Backspace' && !input.value && index) { event.preventDefault(); codeInputs[index-1].value=''; codeInputs[index-1].focus(); }
        if (event.key==='ArrowLeft' && index) { event.preventDefault(); codeInputs[index-1].focus(); }
        if (event.key==='ArrowRight' && index<5) { event.preventDefault(); codeInputs[index+1].focus(); }
    });
});
elements['full-access'].addEventListener('change',async()=>
{
    const enabled=elements['full-access'].checked;
    focusPrompt();
    try
    {
        if (enabled && !state.settings.commands_enabled)
            await api('settings',{commands_enabled:true});
        state=await api('access',{enabled});
    }
    catch (error) { showError(error.message); }
    render();
});
// Reset a submitted draft after acceptance, or immediately when queueing behind an active task.
function clearPrompt()
{
    editingId=null; steerId=null; elements.prompt.value=''; draftAttachments=[];
    elements.prompt.style.height='auto';
}

// Keyboard steering and the visible queue share the same undispatched message order.
function queuedPrompts()
{
    const dispatched=new Set(view.runs.map(run=>run.id));
    return (view.queue || []).filter(input=>input.id!==startingId && !dispatched.has(input.id));
}

// The same composer sends, queues, steers, or interrupts according to the active task.
function renderComposer()
{
    if (!state) return;
    if (startingId)
    {
        const run=view.runs.find(run=>run.id===startingId);
        const accepted=run && state.messages.length>run.start;
        const blocked=view.runs.some(run=>!run.finished_at && run.id!==startingId);
        const queued=blocked && (view.queue || []).some(input=>input.id===startingId);
        if (accepted || queued) { startingId=null; clearPrompt(); }
    }
    elements.prompt.disabled=!state.authenticated;
    elements.prompt.readOnly=queueAction || !!startingId;
    for (const button of elements['quick-actions'].children) button.disabled=elements.prompt.disabled || elements.prompt.readOnly;
    const queue=queuedPrompts(),held=queue.find(input=>input.editing);
    if (held && !editingId && !pendingIds.has(held.id) && !queueAction && !elements.prompt.value && !draftAttachments.length)
    {
        editingId=held.id;
        queueAction=true;
        api('queue/'+held.id+'/edit',{editing:true}).then(input=>{ elements.prompt.value=input.text; draftAttachments=input.attachments || []; }).catch(error=>{ showError(error.message); }).finally(()=>{ queueAction=false; renderComposer(); });
    }
    if (editingId && !queue.some(input=>input.id===editingId)) editingId=null;
    elements['editing-message'].hidden=!editingId;
    elements['cancel-edit'].disabled=queueAction;
    const text=elements.prompt.value.trim(), hasInput=!!text || !!draftAttachments.length, stop=view.busy && !hasInput, queueing=view.busy || !!editingId;
    setIcon(elements.send,stop?'stop':'send');
    elements.send.setAttribute('aria-label',stop?'Stop conversation':queueing?'Queue message':'Send message');
    if (startingId) elements.send.setAttribute('aria-label','Sending message');
    elements.send.disabled=elements.prompt.disabled || elements.prompt.readOnly || attaching || (!view.busy && state.retry_available) || (!stop && !hasInput);
    renderAttachments();
    const inputs=(view.steering || []).filter(input=>input.message_index===undefined), steering=JSON.stringify(inputs);
    if (steering!==steeringKey)
    {
        steeringKey=steering;
        const present=new Set(inputs.map(input=>input.id));
        for (const [id,element] of steeringRows) if (!present.has(id)) { element.remove(); steeringRows.delete(id); }
        for (const input of inputs)
        {
            if (steeringRows.has(input.id)) continue;
            const group=document.createElement('div'), message=document.createElement('article'), text=document.createElement('div');
            group.className='user-message'; message.className='user'; text.className='answer'; text.textContent=input.text;
            message.append(text);
            appendPromptAttachments(group,input.attachments);
            message.hidden=!input.text.trim();
            const actions=document.createElement('div'), copy=document.createElement('button');
            actions.className='prompt-actions'; copy.className='icon-button copy-prompt'; copy.dataset.copySteering=input.id;
            copy.title='Copy message'; copy.setAttribute('aria-label','Copy message'); setIcon(copy,'copy');
            actions.append(copy); group.append(message,actions); steeringRows.set(input.id,group); elements['steering-messages'].append(group);
        }
    }
    elements['queued-messages'].hidden=!queue.some(input=>!input.editing);
    elements['queue-heading'].hidden=elements['queued-messages'].hidden;
    elements['queue-heading'].textContent='Queued · '+queue.filter(input=>!input.editing).length;
    const depleted=state.account && state.account.credits<=0, canSteer=view.can_steer && !depleted;
    const key=JSON.stringify([queue,canSteer,depleted,queueAction]);
    if (key===queueKey) return;
    queueKey=key;
    elements['queued-messages'].replaceChildren();
    for (const input of queue.filter(input=>!input.editing))
    {
        const row=document.createElement('div'), text=document.createElement('span');
        row.innerHTML='<svg aria-hidden="true"><use href="#icon-queue-message"/></svg>';
        text.textContent=[input.text,...(input.attachments || []).map(file=>file.name)].filter(Boolean).join(' · ');
        text.title=text.textContent;
        row.append(text);
        for (const [action,label] of [['steer','Steer'],['edit','Edit queued message'],['remove','Delete queued message']])
        {
            const button=document.createElement('button');
            button.type='button';
            button.dataset.action=action;
            button.dataset.id=input.id;
            button.setAttribute('aria-label',label);
            button.title=action==='steer' && depleted?'Add credits to steer the conversation':label;
            button.disabled=queueAction || (action==='steer' && !canSteer);
            setIcon(button,'queue-'+action);
            if (action==='steer') button.append(document.createTextNode(label));
            row.append(button);
        }
        elements['queued-messages'].append(row);
    }
}

// Idle submissions retain their disabled draft until the native client has accepted them into the chat.
async function submitPrompt(steer=false)
{
    const text=elements.prompt.value, attachments=draftAttachments;
    focusPrompt();
    if (!text.trim() && !attachments.length)
    {
        if (steer)
        {
            const first=queuedPrompts().find(input=>!input.editing);
            if (first) await updateQueuedMessage(first.id,'steer');
            return;
        }
        if (view.busy) try { await api('interrupt',{}); } catch (error) { showError(error.message); }
        return;
    }
    if (elements.send.disabled || elements['input-cost-dialog'].open) return;
    try { if (!await confirmInputCost(text,attachments)) return; }
    catch (error) { showError(error.message); return; }
    if (steer && view.busy && state.account?.credits<=0) { showError('Add credits to steer the conversation.'); return; }
    if (steer && view.busy && !view.can_steer) { showError('No AI turn is accepting steering. Press Enter to queue this message.'); return; }
    const steering=steer && view.busy, draftId=editingId, submittedAt=state.messages.length;
    const id=draftId || (steering?(steerId || crypto.randomUUID()):crypto.randomUUID());
    pendingIds.add(id);
    const direct=!view.busy && !draftId;
    if (direct) startingId=id;
    else clearPrompt();
    showError('');
    renderComposer();
    scrollToEnd();
    try
    {
        await api(steering?'steer':'chat',{id,text,attachments:attachments.length?attachments.map(({kind,name,content})=>({kind,name,content})):null});
        if (startingId===id) { startingId=null; clearPrompt(); }
    }
    catch (error)
    {
        if (startingId===id) startingId=null;
        pendingIds.delete(id);
        showError(error.message);
        const accepted=state.messages.slice(submittedAt).some(message=>message.role==='user' && message.text===text) || (view.queue || []).some(input=>input.id===id && !input.editing);
        if (!elements.prompt.value && !draftAttachments.length && !editingId && !accepted)
        {
            elements.prompt.value=text;
            draftAttachments=attachments;
            editingId=draftId;
            if (steering) steerId=id;
        }
    }
    finally { if (!(view.queue || []).some(input=>input.id===id && input.editing)) pendingIds.delete(id); renderComposer(); }
}
// Start typing at the end of the prompt without taking over dialogs, text fields or shortcuts.
document.addEventListener('keydown',event=>
{
    if (!state?.authenticated || elements.prompt.disabled || elements.prompt.readOnly) return;
    if (event.defaultPrevented || document.querySelector('dialog[open]') || event.target.closest('#questions,#memories-panel')) return;
    const shortcut=(event.ctrlKey || event.metaKey || event.altKey) && !event.getModifierState('AltGraph');
    if (shortcut || event.target.closest('textarea,select,[contenteditable],input:not([type=checkbox]):not([type=radio])')) return;
    if (event.key===' ' && event.target.closest('button,a,summary,input')) return;
    const composing=event.isComposing || ['Dead','Process'].includes(event.key);
    if (!composing && Array.from(event.key).length!==1) return;
    elements.prompt.focus({preventScroll:true});
    const end=elements.prompt.value.length;
    elements.prompt.setSelectionRange(end,end);
    if (composing) return;
    event.preventDefault();
    elements.prompt.setRangeText(event.key,end,end,'end');
    elements.prompt.dispatchEvent(new Event('input',{bubbles:true}));
});

elements.prompt.addEventListener('paste',event=>
{
    const files=[...event.clipboardData.files];
    if (!files.length) return;
    event.preventDefault(); addAttachments(files);
});
elements.attach.addEventListener('click',()=>{focusPrompt(); elements['attachment-input'].click();});
elements['attachment-input'].addEventListener('change',event=>addAttachments([...event.target.files]));
elements.attachments.addEventListener('click',event=>
{
    const button=event.target.closest('[data-attachment]');
    if (!button || attaching || elements.prompt.disabled || elements.prompt.readOnly) return;
    draftAttachments.splice(Number(button.dataset.attachment),1); steerId=null; renderComposer(); focusPrompt();
});
for (const type of ['dragenter','dragover','dragleave','drop']) elements['chat-form'].addEventListener(type,event=>
{
    if (!event.dataTransfer.types.includes('Files')) return;
    event.preventDefault();
    elements['chat-form'].classList.toggle('dragging',type==='dragenter' || type==='dragover');
    if (type==='drop') addAttachments([...event.dataTransfer.files]);
});
for (const type of ['dragover','drop']) window.addEventListener(type,event=>{ if (event.dataTransfer.types.includes('Files')) event.preventDefault(); });
elements['chat-form'].addEventListener('submit',event=>{event.preventDefault(); submitPrompt();});
elements.prompt.addEventListener('keydown',event=>
{
    if (event.key==='Enter' && !event.shiftKey && !event.isComposing)
    {
        event.preventDefault();
        const steer=event.ctrlKey || event.metaKey;
        if (steer || elements.prompt.value.trim() || draftAttachments.length) submitPrompt(steer);
    }
});
// Quick actions prepare an editable draft and use the normal textarea sizing without sending it.
elements['quick-actions'].addEventListener('click',event=>
{
    const button=event.target.closest('button[data-quick-action]');
    if (!button || button.disabled) return;
    elements.prompt.value=view.brand.quick_actions[Number(button.dataset.quickAction)].prompt;
    elements.prompt.dispatchEvent(new Event('input',{bubbles:true}));
    elements.prompt.focus({preventScroll:true});
    elements.prompt.setSelectionRange(elements.prompt.value.length,elements.prompt.value.length);
});
elements.prompt.addEventListener('input',()=>{steerId=null; elements.prompt.style.height='auto'; elements.prompt.style.height=Math.min(162,elements.prompt.scrollHeight)+'px'; renderComposer();});
// Queue buttons and Ctrl+Enter use the same serialized edit, remove and steer operations.
async function updateQueuedMessage(id,action)
{
    if (queueAction || startingId) return;
    if (action==='steer')
    {
        if (state.account?.credits<=0) { showError('Add credits to steer the conversation.'); return; }
        if (!view.can_steer) { showError('No AI turn is accepting steering.'); return; }
    }
    if (action==='edit' && (elements.prompt.value || draftAttachments.length || editingId)) { showError('Send or clear your current draft before editing a queued message.'); elements.prompt.focus(); return; }
    queueAction=true;
    renderComposer(); focusPrompt();
    try
    {
        const result=await api('queue/'+id+'/'+action,action==='edit'?{editing:true}:{});
        if (action==='edit') { editingId=id; elements.prompt.value=result.text; draftAttachments=result.attachments || []; }
    }
    catch (error) { showError(error.message); }
    finally { queueAction=false; renderComposer(); focusPrompt(); }
}
elements['queued-messages'].addEventListener('click',event=>
{
    const button=event.target.closest('[data-action]');
    if (button && !button.disabled) updateQueuedMessage(button.dataset.id,button.dataset.action);
});
elements['cancel-edit'].addEventListener('click',async()=>
{
    if (!editingId || queueAction) return;
    const id=editingId;
    pendingIds.add(id);
    queueAction=true;
    renderComposer(); focusPrompt();
    try { await api('queue/'+id+'/edit',{editing:false}); editingId=null; elements.prompt.value=''; draftAttachments=[]; }
    catch (error) { pendingIds.delete(id); showError(error.message); }
    finally { queueAction=false; renderComposer(); }
});
for (const [id,path] of [['new','new'],['retry','retry']])
    elements[id].addEventListener('click',()=>action(async()=>
    {
        await api(path,{});
    }));
elements['buy-inline'].addEventListener('click',openSettings);
elements['credit-packs'].addEventListener('click',async event=>
{
    const pack=event.target.closest('[data-price-id]');
    if (!pack || buying || !state.account) return;
    // Open during the click so waiting for the backend does not trigger popup blocking.
    const checkoutWindow=window.open('about:blank','_blank');
    if (checkoutWindow)
    {
        checkoutWindow.opener=null;
        checkoutWindow.document.title='Checkout';
        checkoutWindow.document.body.textContent='Opening checkout…';
    }
    buying=true;
    const email=state.account.email;
    render();
    try
    {
        const checkout=await api('checkout',{price_id:pack.dataset.priceId});
        if (state.account?.email!==email) { checkoutWindow?.close(); return; }
        const url=new URL(checkout.url);
        if (url.protocol!=='https:' || url.hostname!=='checkout.stripe.com') throw new Error('Unexpected payment URL');
        if (checkoutWindow && !checkoutWindow.closed) checkoutWindow.location.replace(url.href);
        else location.assign(url.href);
        watchPurchase(checkout.purchase_id,email);
    }
    catch (error) { checkoutWindow?.close(); showError(error.message); elements['settings-status'].textContent=error.message; }
    finally { buying=false; render(); }
});
let selectedProcess, processRecord, processDetailsRequest, processPolling=false, processRefreshPending=false, lastProcessPoll=0, historyKey='';
const processRows=new Map();

// Both command links and table rows clear the previous output as soon as the selection changes.
function selectProcess(id)
{
    processDetailsRequest?.abort();
    selectedProcess=id;
    processRecord=null;
    elements['process-detail'].hidden=true;
}

// The menu toggles a side panel without moving the conversation or blocking process controls.
function toggleProcesses(open)
{
    elements['process-panel'].hidden=!open;
    elements['process-toggle'].setAttribute('aria-expanded',String(open));
    if (open) { toggleMemories(false); elements['process-close'].focus(); pollProcesses(true); }
    else processDetailsRequest?.abort();
}

// Preserve output scroll position while fresh bytes arrive, following only when already at the end.
function updateOutput(element,text)
{
    if (element.textContent===(text || '(empty)')) return;
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
    let result='Running';
    if (record.finished_at) result='Exit code: '+record.exit_code;
    else if (record.background_pids) result='Launcher exit code: '+record.exit_code+' · Background processes running';
    const metadata={'Launcher PID':record.pid || 'Not started',Directory:record.proposal.cwd,Started:new Date(record.started_at).toLocaleString(),
        Finished:record.finished_at?new Date(record.finished_at).toLocaleString():'Still running',
        Result:record.error || result};
    if (record.background_pids?.length) metadata['Background PIDs']=record.background_pids.join(', ');
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

// Command lines can contain HTML syntax, so write their labels as text.
function processButton(record,label)
{
    const button=document.createElement('button');
    button.dataset.select=record.id;
    button.title=record.command;
    button.innerHTML='<time></time><span></span><svg aria-hidden="true"><use href="#icon-chevron"/></svg>';
    button.children[0].textContent=new Date(record.started_at).toLocaleTimeString('en-GB',{hour12:false});
    button.children[1].textContent=label;
    return button;
}

// Poll a separate daemon endpoint so Stop remains available even while the chat lock is held.
async function pollProcesses(refresh=false)
{
    if (processPolling)
    {
        if (refresh) processRefreshPending=true;
        return;
    }
    processPolling=true;
    lastProcessPoll=Date.now();
    try
    {
        const records=(await api('processes',undefined,AbortSignal.timeout(5000))).filter(record=>view.commands.some(command=>command.id===record.id));
        const selected=records.find(record=>record.id===selectedProcess);
        const detailsUnavailable=selectedProcess && !selected;
        if (detailsUnavailable)
        {
            processRecord=null;
            elements['process-detail'].hidden=true;
        }
        const running=records.filter(record=>!record.finished_at);
        elements['process-count'].textContent=running.length+' running';
        elements['process-count'].hidden=!running.length;
        elements['tasks-running'].hidden=!running.length;
        elements['process-toggle'].setAttribute('aria-label',running.length?'Tasks, '+running.length+' running commands':'Tasks');
        elements['process-toggle'].title='Task manager · '+running.length+' running';
        if (elements['process-panel'].hidden) return;
        elements['process-status'].textContent=records.length?'No commands are running.':'No commands in this chat yet.';
        if (running.length) elements['process-status'].textContent='';
        if (detailsUnavailable) elements['process-status'].textContent='Command details are unavailable.';
        elements['process-list'].closest('.task-table-wrap').hidden=!running.length;
        const runningIds=new Set(running.map(record=>record.id));
        for (const [id,row] of processRows)
            if (!runningIds.has(id)) { row.remove(); processRows.delete(id); }
        for (const record of running)
        {
            let row=processRows.get(record.id);
            if (!row)
            {
                row=document.createElement('tr');
                const command=document.createElement('td'), pid=document.createElement('td'), elapsed=document.createElement('td'), action=document.createElement('td');
                const select=processButton(record,record.command), stop=document.createElement('button');
                select.className='process-select';
                stop.className='process-stop'; setIcon(stop,'stop'); stop.append(document.createTextNode('Stop'));
                stop.dataset.stop=record.id;
                stop.setAttribute('aria-label','Kill this command and its child processes');
                command.append(select); action.append(stop); row.append(command,pid,elapsed,action);
                processRows.set(record.id,row);
                elements['process-list'].append(row);
            }
            row.cells[0].firstChild.setAttribute('aria-expanded',String(record.id===selectedProcess));
            row.cells[0].firstChild.setAttribute('aria-pressed',String(record.id===selectedProcess));
            const pids=record.background_pids?.length?record.background_pids:[record.pid].filter(Boolean);
            row.cells[1].textContent=pids[0] || 'Starting';
            if (pids.length>1) row.cells[1].textContent+=' +'+(pids.length-1);
            row.cells[1].title=pids.join(', ');
            row.cells[2].textContent=Math.floor((Date.now()-record.started_at)/1000)+'s';
        }
        const historyPanel=elements['process-history'], details=elements['process-detail'];
        const revealHistory=selected?.finished_at && (details.hidden || !historyPanel.contains(details));
        const history=records.filter(record=>record.finished_at).sort((a,b)=>b.started_at-a.started_at), key=JSON.stringify(history);
        elements['history-heading'].textContent='Completed commands · '+history.length+' · newest first';
        if (key!==historyKey)
        {
            historyKey=key;
            if (historyPanel.contains(details)) details.remove();
            historyPanel.replaceChildren();
            for (const record of history)
            {
                const status=record.error?'Stopped / failed':'Exit '+record.exit_code;
                historyPanel.append(processButton(record,status+' · '+record.command));
            }
        }
        for (const button of historyPanel.querySelectorAll('button[data-select]'))
        {
            const expanded=button.dataset.select===selectedProcess;
            button.setAttribute('aria-expanded',String(expanded));
            button.setAttribute('aria-pressed',String(expanded));
        }
        // Move the single detail section under its selected row instead of duplicating output views.
        if (selected?.finished_at)
        {
            const button=historyPanel.querySelector('[data-select="'+selected.id+'"]');
            if (button.nextElementSibling!==details) button.after(details);
            if (revealHistory) historyPanel.closest('details').open=true;
        }
        else if (selected)
        {
            const row=processRows.get(selected.id);
            let detail=row.nextElementSibling;
            if (!detail?.classList.contains('process-detail-row'))
            {
                detail=document.createElement('tr'); detail.className='process-detail-row';
                const cell=document.createElement('td'); cell.colSpan=4; detail.append(cell); row.after(detail);
            }
            if (details.parentElement!==detail.firstChild) detail.firstChild.append(details);
        }
        for (const row of elements['process-list'].querySelectorAll('.process-detail-row')) if (!row.contains(details)) row.remove();
        const pidsChanged=selected && JSON.stringify(selected.background_pids)!==JSON.stringify(processRecord?.background_pids);
        const changed=selected && (!processRecord || pidsChanged || ['id','pid','finished_at','exit_code','error','stdout_bytes','stderr_bytes'].some(key=>selected[key]!==processRecord[key]));
        if (changed)
        {
            processDetailsRequest=new AbortController();
            const signal=AbortSignal.any([processDetailsRequest.signal,AbortSignal.timeout(5000)]);
            const record=await api('processes/'+selected.id,undefined,signal);
            if (!signal.aborted && selected.id===selectedProcess) renderProcess(record);
        }
    }
    catch (error)
    {
        if (error.name!=='AbortError') elements['process-status'].textContent=error.message;
    }
    finally
    {
        processDetailsRequest=null;
        processPolling=false;
        // An explicit action during a poll gets one fresh snapshot as soon as it finishes.
        if (processRefreshPending)
        {
            processRefreshPending=false;
            pollProcesses();
        }
    }
}

// Both sidebars share resize behavior without moving the chat or blocking its input.
function configureSidebar(prefix,setting)
{
    const panel=elements[prefix+'-panel'], handle=elements[prefix+'-resize'];
    let drag;
    const resize=value=>
    {
        const width=Math.round(Math.min(window.innerWidth,Math.max(288,value)));
        document.documentElement.style.setProperty('--'+setting+'-width',width+'px');
        handle.setAttribute('aria-valuemin',String(Math.min(288,window.innerWidth)));
        handle.setAttribute('aria-valuemax',String(window.innerWidth));
        handle.setAttribute('aria-valuenow',String(width));
    };
    resize(Number(panelWidths.get(setting)) || 32*parseFloat(getComputedStyle(document.documentElement).fontSize));
    handle.addEventListener('pointerdown',event=>
    {
        if (event.button!==0) return;
        event.preventDefault(); drag={x:event.clientX,width:panel.getBoundingClientRect().width};
        handle.setPointerCapture(event.pointerId); document.body.classList.add('resizing-tasks');
    });
    handle.addEventListener('pointermove',event=>{if (drag) resize(drag.width+drag.x-event.clientX);});
    handle.addEventListener('lostpointercapture',()=>
    {
        drag=null; document.body.classList.remove('resizing-tasks');
        panelWidths.set(setting,handle.getAttribute('aria-valuenow'));
    });
    handle.addEventListener('keydown',event=>
    {
        if (!['ArrowLeft','ArrowRight'].includes(event.key)) return;
        event.preventDefault(); resize(panel.getBoundingClientRect().width+(event.key==='ArrowLeft'?32:-32));
        panelWidths.set(setting,handle.getAttribute('aria-valuenow'));
    });
    window.addEventListener('resize',()=>resize(Number(handle.getAttribute('aria-valuenow'))));
}
configureSidebar('process','tasks');
configureSidebar('memories','memories');

// Keep unsaved edits visible until the user saves or explicitly reloads them.
function memoryDirty() { return !!memoryDocument && elements['memories-text'].value!==memoryDocument.text; }

// Opening a sidebar never resets a draft, even while the assistant is working.
function toggleMemories(open)
{
    elements['memories-panel'].hidden=!open;
    elements['memories-toggle'].setAttribute('aria-expanded',String(open));
    if (open)
    {
        toggleProcesses(false);
        if (!memoryDirty()) { loadMemories(); elements['memories-close'].focus(); }
        else elements['memories-text'].focus({preventScroll:true});
    }
}

// Saving freezes only the memory editor; chat, steering and task controls stay usable.
function memoryControls()
{
    elements['memories-text'].disabled=memoryLoading || !memoryDocument;
    elements['memories-text'].readOnly=memorySaving;
    elements['memories-save'].disabled=memoryLoading || memorySaving || !memoryDirty();
    elements['memories-reload'].disabled=memoryLoading || memorySaving;
}

// Request identities discard responses arriving after sign-out or a newer load/save.
async function loadMemories()
{
    if (!state?.authenticated || memoryLoading || memorySaving) return;
    const request=++memoryRequest;
    memoryLoading=true; memoryControls(); elements['memories-status'].textContent='Loading…';
    try
    {
        const result=await api('memories');
        if (request!==memoryRequest) return;
        memoryDocument=result; elements['memories-text'].value=result.text; elements['memories-status'].textContent='';
    }
    catch (error) { if (request===memoryRequest) elements['memories-status'].textContent=error.message; }
    finally
    {
        if (request===memoryRequest)
        {
            memoryLoading=false; memoryControls();
            if (!elements['memories-panel'].hidden && [elements['memories-close'],elements['memories-reload']].includes(document.activeElement)) elements['memories-text'].focus({preventScroll:true});
        }
    }
}
elements['memories-toggle'].addEventListener('click',()=>toggleMemories(elements['memories-panel'].hidden));
elements['memories-close'].addEventListener('click',()=>{toggleMemories(false); elements['memories-toggle'].focus();});
elements['memories-panel'].addEventListener('keydown',event=>{if (event.key==='Escape') { event.stopPropagation(); toggleMemories(false); elements['memories-toggle'].focus(); }});
elements['memories-text'].addEventListener('input',()=>{elements['memories-status'].textContent=memoryDirty()?'Unsaved changes':''; memoryControls();});
elements['memories-reload'].addEventListener('click',()=>{if (!memoryDirty() || confirm('Discard your unsaved edits and load the latest memories?')) loadMemories();});
elements['memories-save'].addEventListener('click',async()=>
{
    if (!memoryDocument || memoryLoading || memorySaving) return;
    const text=elements['memories-text'].value;
    if (new TextEncoder().encode(text).length>16000) { elements['memories-status'].textContent='Keep memories below 16000 UTF-8 bytes. Shorten these notes before saving.'; return; }
    const request=++memoryRequest;
    memorySaving=true; memoryControls(); elements['memories-status'].textContent='Saving…';
    try
    {
        const result=await api('memories',{text,revision:memoryDocument.revision});
        if (request!==memoryRequest) return;
        memoryDocument=result; elements['memories-text'].value=result.text; elements['memories-status'].textContent='Saved';
    }
    catch (error) { if (request===memoryRequest) elements['memories-status'].textContent=error.message; }
    finally
    {
        if (request===memoryRequest)
        {
            memorySaving=false; memoryControls();
            if (!elements['memories-panel'].hidden && [elements['memories-save'],document.body].includes(document.activeElement)) elements['memories-text'].focus({preventScroll:true});
        }
    }
});
elements['process-toggle'].addEventListener('click',()=>toggleProcesses(elements['process-panel'].hidden));
elements['process-close'].addEventListener('click',()=>{toggleProcesses(false); elements['process-toggle'].focus();});
elements['process-panel'].addEventListener('keydown',event=>{if (event.key==='Escape') { toggleProcesses(false); elements['process-toggle'].focus(); }});
elements['process-panel'].addEventListener('click',async event=>
{
    const button=event.target.closest('button');
    if (!button) return;
    if (button.hasAttribute('data-wrap'))
    {
        event.preventDefault();
        const output=elements[button.getAttribute('aria-controls')];
        button.textContent=output.classList.toggle('wrap-output')?'Unwrap':'Wrap';
        return;
    }
    if (button.dataset.select)
    {
        selectProcess(selectedProcess===button.dataset.select?null:button.dataset.select);
        await pollProcesses(true);
    }
    if (button.dataset.stop)
    {
        button.disabled=true;
        try { await api('processes/'+button.dataset.stop+'/stop',{},AbortSignal.timeout(5000)); await pollProcesses(true); }
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


// Idle or hidden tabs avoid constant polling; selected output is fetched only when its metadata changes.
setInterval(()=>
{
    const idle=elements['process-panel'].hidden && !view.busy && elements['tasks-running'].hidden;
    const interval=idle?5000:1000;
    if (state?.authenticated && !document.hidden && Date.now()-lastProcessPoll>=interval) pollProcesses();
},1000);
// Connection attempts remain visible during both sign-in and an active task, including concurrent checkout.
function renderConnection()
{
    const retry=Object.values(view.connections).sort((a,b)=>b.attempt-a.attempt)[0];
    const error=view.backend_error,local=!!elements.status.dataset.connection;
    elements['connection-indicator'].hidden=!retry && !error && !local && !!view.brand;
    let label='Connecting…';
    if (local) label='Reconnecting…';
    else if (retry) label='Reconnecting '+retry.attempt+'/5';
    else if (error) label='Offline';
    elements['connection-indicator'].querySelector('span').textContent=label;
    elements.reconnecting.hidden=!view.busy || !retry;
    elements.reconnecting.querySelector('span').textContent=retry?'Reconnecting '+retry.attempt+'/5':'';
    elements['connection-indicator'].title=local?'Reconnecting to the local assistant':error || 'Connecting to the backend';
    let status='Loading server settings…';
    if (local) status='Reconnecting to the local assistant…';
    else if (error) status=error;
    else if (retry) status='Connecting to the backend…';
    else if (view.brand) status='Loading sign-in options…';
    elements['startup-status'].textContent=status;
    elements['configuration-retry'].hidden=!error || !!view.brand || !!retry;
}
setInterval(()=>{elements.elapsed.textContent=startedAt?Math.floor((Date.now()-startedAt)/1000)+'s':''; renderLogin(); renderConnection();},1000);
// Reserve the actual composer height, including queued messages and a multiline draft, below the transcript.
new ResizeObserver(()=>
{
    document.documentElement.style.setProperty('--composer-height',elements['composer-wrap'].offsetHeight+'px');
    if (followOutput && state?.messages.length) scrollToEnd();
    else renderScrollButton();
}).observe(elements['composer-wrap']);
watch();
// Initial sign-in rendering waits for both local state and available authentication methods.
const initialState=api('state').then(async current=>
{
    state=current;
    render();

    if (!state.authenticated && state.sign_in)
    {
        socialLogin={...state.sign_in};
        renderLogin(); pollSocial();
    }
    if (state.authenticated && !view.busy)
    {
        try { state=await api('account',{}); render(); } catch (error) { showError(error.message); }
    }
}).catch(error=>{showError(error.message);});
const initialProviders=Promise.resolve(JSON.parse(document.getElementById('sign-in-options').textContent)).then(result=>{enabledProviders=result.providers; emailEnabled=result.email_enabled; renderLogin(); if (!emailEnabled && !enabledProviders.length) elements['auth-status'].textContent='Sign-in is disabled. Contact the administrator.';}).catch(error=>{elements['auth-status'].textContent='Social sign-in unavailable: '+error.message;});

// Reveal the complete sign-in screen and focus without shifting its centered position.
Promise.allSettled([initialState,initialProviders]).then(()=>
{
    providersReady=true;
    render();
    if (emailEnabled && view.brand && !state?.authenticated) elements.email.focus({preventScroll:true});
});
