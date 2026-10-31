'use strict';
const elements=Object.fromEntries([...document.querySelectorAll('[id]')].map(element=>[element.id,element]));
let steerId, editingId, queueAction=false, loginBusy=false, buying=false, resendAt=0, state, view={busy:false,steps:[],runs:[],commands:[],connections:{}}, busy=false, acting=false, startedAt, renderFrame, followOutput=true;
const pendingIds=new Set(), messageRows=[], completionRows=[];
let queueKey='', summaryKey='', steeringKey='';
let socialLogin, enabledProviders=[], emailEnabled=false;
const providerNames={google:'Google',microsoft:'Microsoft',github:'GitHub'};

// All icons reference the embedded SVG sprite; no fonts or external assets are needed.
function setIcon(button,name)
{
    if (button.dataset.icon===name) return;
    button.dataset.icon=name;
    button.innerHTML='<svg aria-hidden="true"><use href="#icon-'+name+'"/></svg>';
}
document.documentElement.dataset.theme=localStorage.getItem('assistant-theme') || 'light';
setIcon(elements.theme,document.documentElement.dataset.theme==='dark'?'sun':'moon');
elements.theme.setAttribute('aria-label',document.documentElement.dataset.theme==='dark'?'Switch to light mode':'Switch to dark mode');

// Keep every operation on the authenticated loopback origin.
async function api(path,body)
{
    const response=await fetch('/api/'+path,{method:body===undefined?'GET':'POST',headers:{'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
    if (response.status===413) throw new Error('This request exceeds the HTTP transport size limit (64 MiB). Nothing was sent to the AI.');
    const value=await response.json();
    if (!response.ok) throw new Error(value.error || 'Request failed');
    return value;
}

// Replies use safe Markdown; command output and user input remain plain text.
function render()
{
    if (!state) return;
    busy=view.busy || acting;
    if (view.brand)
    {
        document.title=view.brand.name;
        elements['brand-name'].textContent=view.brand.name;
        elements.brand.setAttribute('aria-label',view.brand.name+' home');
        elements['brand-logo'].hidden=!view.brand.logo;
        elements['brand-mark'].hidden=!!view.brand.logo;
        elements['brand-name'].hidden=!!view.brand.logo && !view.brand.mark;
        elements['brand-logo'].classList.toggle('brand-mark-image',view.brand.mark);
        if (view.brand.logo && elements['brand-logo'].dataset.hash!==view.brand.logo)
        {
            elements['brand-logo'].src='/logo.png'; elements['brand-logo'].alt=view.brand.name; elements['brand-logo'].dataset.hash=view.brand.logo;
        }
    }
    document.body.classList.toggle('signed-out',!state.authenticated);
    elements.setup.hidden=state.authenticated;
    elements['composer-wrap'].hidden=!state.authenticated;
    elements['settings-open'].hidden=!state.authenticated;
    elements['credit-balance'].hidden=!state.authenticated;
    elements['process-toggle'].hidden=!state.authenticated;
    elements.welcome.hidden=!state.authenticated || state.messages.length>0;
    elements['full-access'].checked=state.full_access;
    elements['full-access'].disabled=!state.authenticated || (busy && !state.full_access);
    elements['permission-note'].textContent=state.full_access?'Commands run automatically':state.settings.commands_enabled?'You approve every command':'Local commands are off';
    elements.platform.textContent=(state.platform==='windows'?'Windows · PowerShell':'Linux · sh')+' · Local workspace';
    elements.account.textContent=state.account?.email || 'Sign in to your assistant account';
    elements['settings-balance'].textContent=state.account?state.account.credits.toLocaleString():'Loading…';
    if (elements.purchases.dataset.email!==state.account?.email)
    {
        elements.purchases.replaceChildren();
        elements.purchases.dataset.email=state.account?.email || '';
        elements['purchase-status'].textContent='';
    }
    elements['credit-balance'].textContent=state.account?state.account.credits.toLocaleString()+' credits':'Credits…';
    const depleted=state.account && state.account.credits<=0;
    for (const id of ['credit-balance','settings-balance']) elements[id].classList.toggle('depleted',!!depleted);
    elements['balance-note'].hidden=!depleted;
    if (state.account) { elements['settings-open'].textContent=Array.from(state.account.email)[0].toUpperCase(); delete elements['settings-open'].dataset.icon; }
    else setIcon(elements['settings-open'],'user');
    for (const id of ['new','credits','logout','approve','deny','retry'])
        elements[id].disabled=busy || !state.authenticated;
    elements['buy-inline'].disabled=buying || !state.account;
    for (const button of elements['credit-packs'].querySelectorAll('button')) button.disabled=buying || !state.account || elements['credit-packs'].dataset.development==='true';
    renderConnection();
    renderComposer();
    renderLogin();
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
    const progressByMessage=new Map();
    for (const step of steps)
    {
        if (!progressByMessage.has(step.after)) progressByMessage.set(step.after,[]);
        progressByMessage.get(step.after).push(step);
    }
    // Reuse unchanged messages so expanded output, text selection and keyboard focus survive new replies.
    while (messageRows.length>state.messages.length) messageRows.pop().element.remove();
    const commands=new Map(view.commands.map(command=>[command.after,command]));
    state.messages.forEach((message,index)=>
    {
        const previous=messageRows[index];
        if (message.role==='tool')
        {
            if (!previous || previous.role!=='tool')
            {
                const element=document.createElement('div');
                if (previous) previous.element.replaceWith(element);
                else elements.messages.append(element);
                messageRows[index]={element,role:'tool'};
            }
            if (state.messages[index-1]?.role==='tool')
            {
                if (messageRows[index].details) { messageRows[index].element.replaceChildren(); delete messageRows[index].details; delete messageRows[index].commandKey; }
                return;
            }
            let end=index+1;
            while (state.messages[end]?.role==='tool') end++;
            renderCommandGroup(messageRows[index],index,end,commands);
            return;
        }
        const progress=progressByMessage.get(index) || [], key=JSON.stringify([progress,message.attachments]);
        if (previous?.text===message.text && previous.role===message.role && previous.progress===key) return;
        const group=document.createElement('div');
        if (previous) previous.element.replaceWith(group);
        else elements.messages.append(group);
        messageRows[index]={element:group,text:message.text,role:message.role,progress:key};
        for (const step of progress.filter(step=>step.summaries.length))
        {
            const details=document.createElement('details'), summary=document.createElement('summary');
            details.className='past-progress';
            summary.innerHTML='<span>Thinking'+(step.attempt>1?' · attempt '+step.attempt:'')+'</span><svg aria-hidden="true"><use href="#icon-chevron"/></svg>';
            details.append(summary);
            for (const part of step.summaries)
            {
                const p=document.createElement('p');
                p.textContent=part.text;
                details.append(p);
            }
            group.append(details);
        }
        const article=document.createElement('article');
        article.className=message.role;
        const text=document.createElement('div');
        text.className='answer';
        if (message.role==='assistant') renderAnswer(text,message.text);
        else text.textContent=message.text;
        article.append(text);
        for (const file of message.attachments || []) { const chip=document.createElement('span'); chip.className='attachment sent-attachment'; chip.textContent=file.name; article.append(chip); }
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
    renderCompletions();
    renderCompactions();
    const activity=view.activity;
    const groupedCommand=activity?.progress===null && state.messages.at(-1)?.role==='tool';
    elements.activity.hidden=!activity || !view.busy || groupedCommand;
    elements['activity-label'].classList.toggle('working',view.busy);
    if (activity)
    {
        elements['activity-label'].textContent=activity.label;
        elements['activity-icon'].toggleAttribute('hidden',activity.progress!==null);
        const summaries=activity.progress?.summaries || [], key=JSON.stringify(summaries);
        if (key!==summaryKey)
        {
            summaryKey=key;
            elements.summaries.replaceChildren();
            for (const part of summaries)
            {
                const p=document.createElement('p');
                p.textContent=part.text;
                elements.summaries.append(p);
            }
        }
        renderAnswer(elements['stream-text'],activity.progress?.text || '');
        elements['stream-text'].classList.toggle('streaming',view.busy);
    }
    if (view.error) { elements.status.textContent=view.error; elements.status.dataset.taskError='true'; }
    else if (elements.status.dataset.taskError) { elements.status.textContent=''; delete elements.status.dataset.taskError; }
    renderScrollButton();
}

// Adjacent command events share a stable disclosure; conversation text starts a new group.
function renderCommandGroup(row,start,end,commands)
{
    if (!row.details)
    {
        row.details=document.createElement('details');
        row.details.className='tool-result';
        const summary=document.createElement('summary');
        summary.innerHTML='<svg aria-hidden="true"><use href="#icon-terminal"/></svg><span></span><svg class="disclosure-arrow" aria-hidden="true"><use href="#icon-chevron"/></svg>';
        row.label=summary.querySelector('span');
        row.activity=document.createElement('p');
        row.activity.className='command-activity';
        row.body=document.createElement('div');
        row.details.append(summary,row.activity,row.body);
        row.element.append(row.details);
    }
    const active=view.busy && end===state.messages.length && commands.has(end-1);
    const grouped=view.commands.filter(command=>command.after>=start && command.after<end);
    row.label.textContent='Command details';
    if (grouped.length) row.label.textContent=active?'Running commands…':'Ran commands';
    row.label.classList.toggle('working',active);
    const activity=active && view.activity?.progress===null?view.activity.label:'';
    row.activity.hidden=!activity;
    row.activity.textContent=activity;
    const changed=grouped.flatMap(command=>command.changes || []);
    if (changed.length) row.label.textContent=active?'Editing files, running commands…':'Edited files, ran commands';
    row.details.querySelector('summary use').setAttribute('href',changed.length?'#icon-edit':'#icon-terminal');
    if (!row.actions) row.actions=new Map();
    const present=new Set();
    row.actionPosition=0;
    for (let index=start;index<end;index++)
    {
        const command=commands.get(index);
        const result=command && index+1<end && !commands.has(index+1)?state.messages[++index].text:'';
        if (command)
        {
            for (const change of command.changes || [])
            {
                const key=command.id+':'+change.path;
                present.add(key);
                updateAction(row,key,'edit',change.kind+' '+change.path,change.diff,null,change);
            }
        }
        const key=command?.id || 'output:'+index;
        present.add(key);
        updateAction(row,key,'terminal',command?(result?'Ran ':'Running ')+command.command:'Command result',
            command?command.command+'\nDirectory: '+command.cwd+'\n\n'+(result || 'Waiting for the command result…'):state.messages[index].text,command?.id);
    }
    for (const [key,item] of row.actions) if (!present.has(key)) { item.element.remove(); row.actions.delete(key); }
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
        element.append(summary,pre);
        if (process)
        {
            const button=document.createElement('button'); button.className='quiet'; button.dataset.process=process; button.textContent='Open in task manager'; element.append(button);
        }
        item={element,summary,pre}; row.actions.set(key,item); row.body.append(element);
    }
    const position=row.body.children[row.actionPosition++];
    if (position!==item.element) row.body.insertBefore(item.element,position || null);
    item.summary.querySelector('span').textContent=label;
    item.summary.title=label;
    if (item.pre.textContent!==output) item.pre.textContent=output;
    if (change && change.added!==null && !item.count)
    {
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

// Completed tasks keep their own timestamp and interrupted text, even when the next queued task starts.
function renderCompletions()
{
    const runs=view.runs.filter(run=>run.finished_at), anchors=new Map();
    while (completionRows.length>runs.length) completionRows.pop().element.remove();
    runs.forEach((run,index)=>
    {
        const key=JSON.stringify(run);
        let row=completionRows[index];
        if (row?.key!==key)
        {
            const element=document.createElement('section');
            element.className='completion';
            if (run.summaries.length)
            {
                const details=document.createElement('details'), summary=document.createElement('summary');
                details.className='past-progress';
                summary.innerHTML='<span>Thinking</span><svg aria-hidden="true"><use href="#icon-chevron"/></svg>';
                details.append(summary);
                for (const part of run.summaries) { const paragraph=document.createElement('p'); paragraph.textContent=part.text; details.append(paragraph); }
                element.append(details);
            }
            if (run.partial)
            {
                const partial=document.createElement('div');
                partial.className='answer partial-answer';
                renderAnswer(partial,run.partial);
                element.append(partial);
            }
            const footer=document.createElement('div'), copy=document.createElement('button'), time=document.createElement('time'), status=document.createElement('span');
            footer.className='completion-footer';
            copy.className='icon-button copy-answer';
            copy.dataset.copy=index;
            copy.title='Copy response';
            copy.setAttribute('aria-label','Copy response');
            copy.disabled=!run.partial && !state.messages.slice(run.start,run.end).some(message=>message.role==='assistant' && message.text);
            setIcon(copy,'copy');
            const date=new Date(run.finished_at);
            time.dateTime=date.toISOString();
            time.textContent=date.toLocaleTimeString([],{hour:'2-digit',minute:'2-digit'});
            time.title=date.toLocaleString();
            status.textContent=run.error || 'Completed';
            const seconds=Math.max(0,Math.round((run.finished_at-run.started_at)/1000));
            status.title='Worked for '+Math.floor(seconds/60)+'m '+seconds%60+'s';
            footer.append(copy,time,status);
            element.append(footer);
            if (row) row.element.replaceWith(element);
            completionRows[index]=row={element,key};
        }
        const anchor=anchors.get(run.end) || messageRows[run.end-1]?.element;
        if (anchor && row.element.previousElementSibling!==anchor) anchor.after(row.element);
        else if (!anchor && elements.messages.firstElementChild!==row.element) elements.messages.prepend(row.element);
        anchors.set(run.end,row.element);
    });
}

// Copy the final response, or its interrupted preview, without commands or progress chatter.
elements.messages.addEventListener('click',async event=>
{
    const copy=event.target.closest('[data-copy],[data-copy-prompt]'), process=event.target.closest('[data-process]');
    if (copy)
    {
        const prompt=copy.hasAttribute('data-copy-prompt'), label=prompt?'Copy message':'Copy response';
        let text;
        if (prompt) text=state.messages[Number(copy.dataset.copyPrompt)].text;
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
        catch (error) { elements.status.textContent='Could not copy: '+error.message; }
    }
    if (process)
    {
        selectedProcess=process.dataset.process;
        processRecord=null;
        toggleProcesses(true);
    }
});

// Show a way back to the latest output whenever the reader is away from the end.
function renderScrollButton()
{
    elements['scroll-bottom'].hidden=!state?.authenticated || document.documentElement.scrollHeight-window.scrollY-window.innerHeight<=4;
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
            if (elements.status.dataset.connection) { elements.status.textContent=''; delete elements.status.dataset.connection; }
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
            elements.status.textContent='Reconnecting to the local assistant… '+error.message;
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
    setIcon(elements.theme,dark?'sun':'moon');
    elements.theme.setAttribute('aria-label',dark?'Switch to light mode':'Switch to dark mode');
});
// Keep purchase history authoritative and scoped to the account that opened Settings.
async function loadPurchases()
{
    const email=state.account?.email;
    if (!email) return;
    elements['purchase-status'].textContent='Loading purchases…';
    elements.purchases.replaceChildren();
    try
    {
        const result=await api('purchases');
        if (state.account?.email!==email) return;
        const rows=document.createDocumentFragment();
        for (const purchase of result.purchases)
        {
            const row=document.createElement('li'),credits=document.createElement('strong'),time=document.createElement('time');
            credits.textContent='+'+purchase.credits.toLocaleString()+' credits';
            time.dateTime=new Date(purchase.paid_at*1000).toISOString();
            time.textContent=new Date(purchase.paid_at*1000).toLocaleString();
            row.append(credits,time); rows.append(row);
        }
        elements.purchases.replaceChildren(rows);
        elements['purchase-status'].textContent=result.purchases.length?'':'No completed purchases yet.';
    }
    catch (error) { if (state.account?.email===email) elements['purchase-status'].textContent=error.message; }
}

// Fetch purchase choices whenever Settings opens so administrator changes appear without restarting the client.
async function loadCreditPacks()
{
    const email=state.account?.email;
    if (!email) return;
    elements['credit-packs'].replaceChildren();
    elements['credit-packs-status'].textContent='Loading credit packs…';
    try
    {
        const result=await api('credit-packs');
        if (state.account?.email!==email) return;
        elements['credit-packs'].dataset.development=String(result.development);
        for (const pack of result.packs)
        {
            const button=document.createElement('button'),credits=document.createElement('strong'),price=document.createElement('span');
            button.type='button'; button.className='credit-pack'; button.dataset.priceId=pack.price_id;
            credits.textContent=pack.credits.toLocaleString()+' credits';
            price.textContent=new Intl.NumberFormat(undefined,{style:'currency',currency:'USD'}).format(Number(pack.usd))+' USD';
            button.append(credits,price); elements['credit-packs'].append(button);
        }
        elements['credit-packs-status'].textContent=result.development?'Purchases are disabled in development mode.':result.packs.length?'Choose a pack to open checkout.':'No credit packs are currently available.';
        render();
    }
    catch (error) { if (state.account?.email===email) elements['credit-packs-status'].textContent=error.message; }
}

// Each opening starts with the credit overview; purchase history is loaded only when expanded.
function openSettings()
{
    elements['settings-status'].textContent='';
    elements['purchase-history'].hidden=true;
    elements['toggle-purchases'].setAttribute('aria-expanded','false');
    elements['settings-dialog'].showModal();
    loadCreditPacks();
}
elements['settings-open'].addEventListener('click',openSettings);
elements['settings-close'].addEventListener('click',()=>elements['settings-dialog'].close());
elements['credit-balance'].addEventListener('click',openSettings);
// Derive the toggle state from the section instead of keeping a separate visibility flag.
elements['toggle-purchases'].addEventListener('click',()=>
{
    const expanded=elements['purchase-history'].hidden;
    elements['purchase-history'].hidden=!expanded;
    elements['toggle-purchases'].setAttribute('aria-expanded',String(expanded));
    if (expanded) loadPurchases();
});
const codeInputs=[...elements['code-digits'].querySelectorAll('input')];

// Keep the sign-in form local, including resend timing and development-only delivery hints.
function renderLogin()
{
    const remaining=Math.max(0,Math.ceil((resendAt-Date.now())/1000));
    const waiting=!!socialLogin && !socialLogin.verification;
    const emailAllowed=emailEnabled || !!socialLogin?.verification;
    elements['email-sign-in'].hidden=!emailAllowed;
    elements['auth-divider'].hidden=!emailAllowed;
    for (const id of ['email','login','verify','change-email']) elements[id].disabled=loginBusy || waiting || !emailAllowed;
    elements['code-digits'].disabled=loginBusy;
    elements.resend.disabled=loginBusy || remaining>0;
    elements.resend.textContent=remaining?'Resend in '+remaining+'s':'Resend code';
    elements.login.textContent=loginBusy?'Signing in…':'Sign in with email';
    elements['social-providers'].hidden=!enabledProviders.length || !!socialLogin?.verification || !elements['code-form'].hidden;
    for (const button of document.querySelectorAll('[data-provider]'))
    {
        button.hidden=!enabledProviders.includes(button.dataset.provider);
        button.disabled=loginBusy || !!socialLogin;
    }
    elements['cancel-sign-in'].hidden=!socialLogin;
    elements['cancel-sign-in'].disabled=loginBusy;
}

// Email and social sign-in share one success transition, including clearing old session errors.
function signedIn(current)
{
    socialLogin?.popup?.close();
    socialLogin=undefined;
    state=current;
    view.error=null;
    elements.status.textContent='';
    delete elements.status.dataset.taskError;
    elements['auth-status'].textContent='';
    elements['code-form'].hidden=true;
    elements['email-form'].hidden=false;
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
            elements['cancel-sign-in'].click();
            return;
        }
    }
    catch (error)
    {
        if (socialLogin!==attempt) return;
        attempt.popup?.close();
        try { await api('login/cancel',{id:attempt.id}); if (socialLogin===attempt) socialLogin=undefined; }
        catch { /* Keep Cancel available if the backend is temporarily unreachable. */ }
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
    elements['auth-status'].textContent='Continue with '+providerNames[attempt.provider]+' in the sign-in window…';
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
elements['cancel-sign-in'].addEventListener('click',async()=>
{
    if (loginBusy || !socialLogin?.id) return;
    loginBusy=true; renderLogin();
    try
    {
        await api('login/cancel',{id:socialLogin.id});
        socialLogin.popup?.close(); socialLogin=undefined;
        elements['email-form'].hidden=false; elements['code-form'].hidden=true;
        elements['auth-status'].textContent='';
        elements.email.focus();
    }
    catch (error) { elements['auth-status'].textContent=error.message; }
    finally { loginBusy=false; renderLogin(); }
});

// Both the initial request and resend use the same email and never navigate away.
async function requestCode()
{
    if (loginBusy || !elements['email-form'].reportValidity()) return;
    loginBusy=true;
    elements['auth-status'].textContent='';
    renderLogin();
    try
    {
        const result=await api('login',{email:elements.email.value.trim()});
        resendAt=Date.now()+60000;
        elements['email-form'].hidden=true;
        elements['code-form'].hidden=false;
        elements['login-email'].textContent=elements.email.value.trim();
        elements['development-code'].hidden=!result.development_code;
        elements['development-code'].textContent=result.development_code?'Development code: '+result.development_code:'';
        for (const input of codeInputs) input.value='';
    }
    catch (error) { elements['auth-status'].textContent=error.message; }
    finally { loginBusy=false; renderLogin(); if (!elements['code-form'].hidden) codeInputs[0].focus(); }
}
elements['email-form'].addEventListener('submit',event=>{event.preventDefault(); requestCode();});
elements.resend.addEventListener('click',requestCode);
elements['change-email'].addEventListener('click',()=>
{
    elements['email-form'].hidden=false;
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
    try
    {
        if (enabled && !state.settings.commands_enabled)
            await api('settings',{commands_enabled:true});
        state=await api('access',{enabled});
    }
    catch (error) { elements.status.textContent=error.message; }
    render();
});
// The same composer sends, queues, steers, or interrupts according to the active task.
function renderComposer()
{
    if (!state) return;
    elements.prompt.disabled=!state.authenticated || queueAction;
    const queue=view.queue || [], held=queue.find(input=>input.editing);
    if (held && !editingId && !pendingIds.has(held.id) && !queueAction && !elements.prompt.value && !draftAttachments.length)
    {
        editingId=held.id;
        queueAction=true;
        api('queue/'+held.id+'/edit',{editing:true}).then(input=>{ elements.prompt.value=input.text; draftAttachments=input.attachments || []; }).catch(error=>{ elements.status.textContent=error.message; }).finally(()=>{ queueAction=false; renderComposer(); });
    }
    if (editingId && !queue.some(input=>input.id===editingId)) editingId=null;
    elements['editing-message'].hidden=!editingId;
    elements['cancel-edit'].disabled=queueAction;
    const text=elements.prompt.value.trim(), hasInput=!!text || !!draftAttachments.length, stop=view.busy && !hasInput, queueing=view.busy || !!state.pending || !!editingId;
    setIcon(elements.send,stop?'stop':'send');
    elements.send.setAttribute('aria-label',stop?'Stop conversation':queueing?'Queue message':'Send message');
    elements.send.disabled=!state.authenticated || queueAction || attaching || (!view.busy && state.retry_available) || (!stop && !hasInput);
    renderAttachments();
    const steering=JSON.stringify(view.steering || []);
    if (steering!==steeringKey)
    {
        steeringKey=steering;
        elements['steering-messages'].replaceChildren();
        for (const input of view.steering || [])
        {
            const message=document.createElement('p');
            message.textContent='You · '+[input.text,...(input.attachments || []).map(file=>file.name)].filter(Boolean).join(' · ');
            elements['steering-messages'].append(message);
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
            if (action==='steer') button.textContent=label;
            else setIcon(button,action);
            row.append(button);
        }
        elements['queued-messages'].append(row);
    }
}

// Clear each submitted draft immediately so another message can be queued during generation.
async function submitPrompt(steer=false)
{
    const text=elements.prompt.value, attachments=draftAttachments;
    if (!text.trim() && !attachments.length)
    {
        if (view.busy) try { await api('interrupt',{}); } catch (error) { elements.status.textContent=error.message; }
        return;
    }
    if (elements.send.disabled || elements['input-cost-dialog'].open) return;
    try { if (!await confirmInputCost(text,attachments)) return; }
    catch (error) { elements.status.textContent=error.message; return; }
    if (steer && view.busy && state.account?.credits<=0) { elements.status.textContent='Add credits to steer the conversation.'; return; }
    if (steer && view.busy && !view.can_steer) { elements.status.textContent='No AI turn is accepting steering. Press Enter to queue this message.'; return; }
    const steering=steer && view.busy, draftId=editingId, submittedAt=state.messages.length;
    const id=draftId || (steering?(steerId || crypto.randomUUID()):crypto.randomUUID());
    pendingIds.add(id);
    editingId=null;
    steerId=null;
    elements.prompt.value='';
    draftAttachments=[];
    elements.prompt.style.height='auto';
    elements.status.textContent='';
    renderComposer();
    scrollToEnd();
    try { await api(steering?'steer':'chat',{id,text,attachments:attachments.length?attachments.map(({kind,name,content})=>({kind,name,content})):null}); }
    catch (error)
    {
        pendingIds.delete(id);
        elements.status.textContent=error.message;
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
elements.prompt.addEventListener('paste',event=>
{
    const files=[...event.clipboardData.files];
    if (!files.length) return;
    event.preventDefault(); addAttachments(files);
});
elements.attach.addEventListener('click',()=>elements['attachment-input'].click());
elements['attachment-input'].addEventListener('change',event=>addAttachments([...event.target.files]));
elements.attachments.addEventListener('click',event=>
{
    const button=event.target.closest('[data-attachment]');
    if (!button || attaching) return;
    draftAttachments.splice(Number(button.dataset.attachment),1); steerId=null; renderComposer();
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
    if (event.key==='Enter' && !event.shiftKey && !event.isComposing) { event.preventDefault(); submitPrompt(event.ctrlKey || event.metaKey); }
});
elements.prompt.addEventListener('input',()=>{steerId=null; elements.prompt.style.height='auto'; elements.prompt.style.height=Math.min(162,elements.prompt.scrollHeight)+'px'; renderComposer();});
elements['queued-messages'].addEventListener('click',async event=>
{
    const button=event.target.closest('[data-action]');
    if (!button || queueAction || button.disabled) return;
    const {id,action}=button.dataset;
    if (action==='edit' && (elements.prompt.value || draftAttachments.length || editingId)) { elements.status.textContent='Send or clear your current draft before editing a queued message.'; elements.prompt.focus(); return; }
    queueAction=true;
    renderComposer();
    try
    {
        const result=await api('queue/'+id+'/'+action,action==='edit'?{editing:true}:{});
        if (action==='edit') { editingId=id; elements.prompt.value=result.text; draftAttachments=result.attachments || []; }
    }
    catch (error) { elements.status.textContent=error.message; }
    finally { queueAction=false; renderComposer(); if (action==='edit' && editingId===id) elements.prompt.focus(); }
});
elements['cancel-edit'].addEventListener('click',async()=>
{
    if (!editingId || queueAction) return;
    const id=editingId;
    pendingIds.add(id);
    queueAction=true;
    renderComposer();
    try { await api('queue/'+id+'/edit',{editing:false}); editingId=null; elements.prompt.value=''; draftAttachments=[]; }
    catch (error) { pendingIds.delete(id); elements.status.textContent=error.message; }
    finally { queueAction=false; renderComposer(); }
});
for (const id of ['approve','deny']) elements[id].addEventListener('click',()=>
{
    const proposalId=state.pending.id;
    if (id==='approve') selectedProcess=proposalId;
    action(()=>api('command',{id:proposalId,approve:id==='approve'}));
});
for (const [id,path] of [['new','new'],['credits','account'],['retry','retry']])
    elements[id].addEventListener('click',()=>action(async()=>
    {
        await api(path,{});
        if (id==='credits')
        {
            if (!elements['purchase-history'].hidden) await loadPurchases();
            await loadCreditPacks();
        }
    }));
// Reload after session revocation so login starts with a fresh form and no previous draft.
elements.logout.addEventListener('click',async()=>
{
    if (await action(()=>api('logout',{}))) location.reload();
});
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
    }
    catch (error) { checkoutWindow?.close(); elements.status.textContent=error.message; elements['settings-status'].textContent=error.message; }
    finally { buying=false; render(); }
});
let selectedProcess, processPolling=false, processRecord, lastProcessPoll=0, historyKey='';
const processRows=new Map();

// Keep the floating panel usable while conversation requests are waiting on commands or AI.
function toggleProcesses(open)
{
    elements['process-panel'].hidden=!open;
    elements['process-toggle'].setAttribute('aria-expanded',String(open));
    if (open) { elements['process-close'].focus(); pollProcesses(); }
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
    lastProcessPoll=Date.now();
    try
    {
        const records=await api('processes');
        if (selectedProcess && !records.some(record=>record.id===selectedProcess))
        {
            processRecord=null;
            selectedProcess=null;
            elements['process-detail'].hidden=true;
        }
        const running=records.filter(record=>!record.finished_at);
        elements['process-count'].textContent=running.length+' running';
        elements['process-toggle'].title='Task manager · '+running.length+' running';
        if (elements['process-panel'].hidden) return;
        elements['process-status'].textContent=running.length?'':'No commands are running.';
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
        const history=records.filter(record=>record.finished_at), key=JSON.stringify(history);
        elements['history-heading'].textContent='Completed commands · '+history.length;
        if (key!==historyKey)
        {
            historyKey=key;
            elements['process-history'].replaceChildren();
            for (const record of history)
            {
                const button=document.createElement('button');
                button.dataset.select=record.id;
                button.textContent=(record.error?'Stopped / failed':('Exit '+record.exit_code))+' · '+record.command;
                button.title=record.command;
                elements['process-history'].append(button);
            }
        }
        for (const button of elements['process-history'].children) button.setAttribute('aria-pressed',String(button.dataset.select===selectedProcess));
        const selected=records.find(record=>record.id===selectedProcess);
        const changed=selected && (!processRecord || ['id','pid','finished_at','exit_code','error','stdout_bytes','stderr_bytes'].some(key=>selected[key]!==processRecord[key]));
        if (changed)
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


// Idle or hidden tabs avoid constant polling; selected output is fetched only when its metadata changes.
setInterval(()=>
{
    const interval=elements['process-panel'].hidden && !view.busy?5000:1000;
    if (state?.authenticated && !document.hidden && Date.now()-lastProcessPoll>=interval) pollProcesses();
},1000);
// Connection attempts remain visible during both sign-in and an active task, including concurrent checkout.
function renderConnection()
{
    const retry=Object.values(view.connections).sort((a,b)=>b.attempt-a.attempt)[0];
    for (const id of ['backend-connection','auth-connection'])
    {
        elements[id].hidden=!retry;
        if (!retry) continue;
        const seconds=Math.max(0,Math.ceil((retry.retry_at-Date.now())/1000));
        elements[id].textContent=(seconds?'Backend unavailable. Retrying in '+seconds+'s':'Connecting to backend…')+' · attempt '+retry.attempt+' of 5';
    }
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
// Either startup request may finish first; focus when the email field becomes available.
api('state').then(async current=>
{
    state=current;
    render();
    if (!state.authenticated && state.sign_in)
    {
        socialLogin={...state.sign_in};
        elements['auth-status'].textContent='Waiting for '+providerNames[socialLogin.provider]+' sign-in…';
        renderLogin(); pollSocial();
    }
    if (!state.authenticated && emailEnabled) elements.email.focus();
    if (state.authenticated && !view.busy)
    {
        try { state=await api('account',{}); render(); } catch (error) { elements.status.textContent=error.message; }
    }
}).catch(error=>{elements.status.textContent=error.message;});
api('login/providers').then(result=>{enabledProviders=result.providers; emailEnabled=result.email_enabled; renderLogin(); if (emailEnabled && !state?.authenticated) elements.email.focus(); if (!emailEnabled && !enabledProviders.length) elements['auth-status'].textContent='Sign-in is disabled. Contact the administrator.';}).catch(error=>{elements['auth-status'].textContent='Social sign-in unavailable: '+error.message;});
