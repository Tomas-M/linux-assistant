'use strict';
const questionViews=new Map();

// Keep native disclosure semantics, and leave content visible until a closing slide finishes.
function enableDisclosureAnimation(details,prepare)
{
    let animation;
    const summary=details.firstElementChild;
    summary.addEventListener('click',event=>
    {
        event.preventDefault();
        const start=details.getBoundingClientRect().height,opening=!details.open || details.classList.contains('disclosure-closing');
        animation?.cancel();
        details.classList.toggle('disclosure-closing',!opening);
        if (opening) { details.open=true; prepare?.(); }
        if (matchMedia('(prefers-reduced-motion: reduce)').matches)
        {
            details.open=opening; details.classList.remove('disclosure-closing'); details.style.overflow=''; animation=null; return;
        }
        details.style.overflow='clip';
        const style=getComputedStyle(details);
        const closedHeight=summary.getBoundingClientRect().height+parseFloat(style.paddingTop)+parseFloat(style.paddingBottom)+parseFloat(style.borderTopWidth)+parseFloat(style.borderBottomWidth);
        const end=opening?details.getBoundingClientRect().height:closedHeight;
        const transition=details.animate({height:[start+'px',end+'px']},{duration:220,easing:'cubic-bezier(0.2,0,0,1)'});
        animation=transition;
        transition.onfinish=()=>
        {
            if (animation!==transition) return;
            details.open=opening; transition.cancel(); animation=null;
            details.classList.remove('disclosure-closing'); details.style.overflow='';
        };
    });
}

// Use the same semantic icon for an action and its progress-group heading.
function progressIcon(part)
{
    const tool=part.data?.tool;
    if (part.kind==='reasoning') return 'lightbulb';
    if (part.kind==='web_search') return 'globe';
    if (tool==='local_search_files') return 'search';
    if (['local_exec_command','local_write_stdin','local_stop_process'].includes(tool)) return 'terminal';
    if (['local_view_image','generate_image'].includes(tool)) return 'image';
    if (['memory_read','memory_save'].includes(tool)) return 'layer-group';
    if (tool==='local_list_directory') return 'folder';
    if (['local_read_file','local_get_metadata'].includes(tool)) return 'file';
    if (['local_apply_patch','local_write_file','local_create_directory','local_copy_path','local_move_path','local_remove_path','local_create_symlink','local_set_permissions'].includes(tool)) return 'edit';
    return 'list-ul';
}

// Commentary, plans and steering end a consecutive activity group; each group keeps its own disclosure.
function renderProgress(container,parts,active=false,steers=[])
{
    container.classList.add('progress-list');
    let rows=progressViews.get(container);
    if (!rows) { rows=new Map(); progressViews.set(container,rows); }
    const groups=[];
    for (let index=0;index<=parts.length;index++)
    {
        for (const input of steers.filter(input=>Math.min(input.progress_after,parts.length)===index)) groups.push({key:'steer:'+input.id,kind:'steer',input});
        const part=parts[index];
        if (!part?.text.trim()) continue;
        const work=['reasoning','action','web_search'].includes(part.kind), previous=groups.at(-1);
        if (work && previous?.kind==='work') previous.parts.push(part);
        else groups.push({key:part.id,kind:work?'work':part.kind,parts:[part]});
    }
    const present=new Set();
    groups.forEach((group,index)=>
    {
        let row=rows.get(group.key);
        if (!row)
        {
            const element=group.kind==='steer'?steeringRows.get(group.input.id):document.createElement('div');
            let label,disclosure,body=element;
            if (group.kind==='work')
            {
                element.className='activity-group'; disclosure=document.createElement('details'); disclosure.className='tool-result activity-stack';
                const summary=document.createElement('summary');
                summary.innerHTML='<svg aria-hidden="true"><use href="#icon-terminal"/></svg><span class="activity-latest"></span><span class="activity-collapse">Hide activities</span><small class="action-failures" hidden></small><svg class="disclosure-arrow" aria-hidden="true"><use href="#icon-chevron"/></svg>';
                label=summary.querySelector('span'); body=document.createElement('div');
                disclosure.append(summary,body); element.append(disclosure);
            }
            else if (group.kind==='plan') element.className='live-plan';
            else if (group.kind!=='steer') element.className='answer progress-commentary';
            row={element,label,body,disclosure,actions:new Map(),reasoning:new Map()}; rows.set(group.key,row);
            if (group.kind==='work')
            {
                const update=()=>renderWork(row,row.parts,row.active);
                disclosure.addEventListener('toggle',update); enableDisclosureAnimation(disclosure,update);
            }
        }
        if (group.kind==='work') renderWork(row,group.parts,active && group.parts.some(part=>!part.completed));
        else if (group.kind==='plan')
        {
            const part=group.parts[0], key=JSON.stringify(part.data);
            if (row.key!==key)
            {
                row.key=key; row.body.replaceChildren();
                const heading=document.createElement('strong'); heading.textContent='Plan'; row.body.append(heading);
                if (part.data.explanation) { const text=document.createElement('p'); text.textContent=part.data.explanation; row.body.append(text); }
                const list=document.createElement('ol');
                for (const step of part.data.plan)
                {
                    const item=document.createElement('li'), icon=document.createElement('span'), text=document.createElement('span');
                    item.dataset.status=step.status; icon.className='plan-state';
                    if (step.status==='completed') setIcon(icon,'check');
                    else icon.textContent=String(list.children.length+1);
                    text.textContent=step.step; item.append(icon,text); list.append(item);
                }
                row.body.append(list);
            }
        }
        else if (group.kind==='commentary') renderAnswer(row.body,group.parts.map(part=>part.text).join('\n\n'));
        present.add(group.key);
        if (container.children[index]!==row.element) container.insertBefore(row.element,container.children[index] || null);
    });
    for (const [key,row] of rows) if (!present.has(key)) { if (row.element.parentElement===container) row.element.remove(); rows.delete(key); }
}

// Keep call identity and source order separate from compact previews, as Codex's ActivityGroup does.
function progressCalls(parts)
{
    const calls=new Map(),sessions=new Map();
    for (const part of parts.filter(part=>part.kind!=='reasoning'))
    {
        const session=view.tool_results[part.id]?.session_id || part.data?.arguments?.session_id || part.id;
        if (!sessions.has(session))
        {
            const call={parts:[]}; sessions.set(session,call); calls.set(part.id,call);
        }
        sessions.get(session).parts.push(part);
    }
    return calls;
}

// Format structured actions without guessing what an arbitrary shell command does.
function progressAction(parts)
{
    const first=parts[0],last=parts.at(-1),result=view.tool_results[last.id],args=first.data?.arguments || {},tool=first.data?.tool;
    const running=result?.running ?? !last.completed,failed=result?.success===false || last.data?.status==='failed';
    const output=parts.map(part=>view.tool_results[part.id]?.output || '').filter(Boolean).join('\n\n');
    let label=last.text,icon=progressIcon(first);
    if (tool==='local_exec_command') label=(running?'Running ':'Ran ')+args.cmd;
    if (tool==='local_read_file') label=(running?'Reading ':'Read ')+args.path;
    if (tool==='local_list_directory') label=(running?'Listing ':'Listed ')+args.path;
    if (tool==='local_search_files') label=(running?'Searching ':'Searched ')+args.path;
    if (tool==='local_get_metadata') label=(running?'Inspecting ':'Inspected ')+args.path;
    if (tool==='local_apply_patch') label=(running?'Editing ':'Edited ')+args.path;
    if (tool==='local_write_file') label=(running?'Creating ':'Created ')+args.path;
    if (tool==='local_view_image') label=(running?'Viewing image: ':'Viewed image: ')+args.path;
    if (first.kind==='web_search')
    {
        const action=last.data?.action;
        if (action?.type==='openPage') label=(running?'Opening ':'Opened ')+action.url;
        else if (action?.type==='findInPage') label=(running?'Finding ':'Found ')+action.pattern+' in '+action.url;
        else if (action?.query) label=(running?'Searching the web: ':'Searched the web: ')+action.query;
    }
    const changes=new Map();
    for (const part of parts) for (const change of view.tool_results[part.id]?.changes || []) changes.set(change.path,change);
    if (changes.size) icon='edit';
    if (failed) label='Failed: '+(args.cmd || last.text);
    return {label,icon,output,failed,running,changes,tool,args,process:result?.session_id || args.session_id};
}

// A group previews only its latest activity; its top collapse control is separate from every row's details.
function renderWork(row,parts,active)
{
    row.parts=parts; row.active=active;
    const calls=progressCalls(parts),actions=new Map([...calls].map(([id,call])=>[id,progressAction(call.parts)]));
    const latest=parts.at(-1),call=[...calls].find(([,call])=>call.parts.includes(latest));
    const current=call && actions.get(call[0]);
    let title=progressTitle([latest]) || 'Thinking',icon=progressIcon(latest);
    if (current) { title=current.label; icon=current.icon; }
    if (row.label.textContent!==title) row.label.textContent=title;
    row.label.title=title; row.label.classList.toggle('working',active);
    row.label.classList.toggle('action-failed',current?.failed===true);
    const use=row.disclosure.querySelector('summary use');
    if (use.getAttribute('href')!=='#icon-'+icon) use.setAttribute('href','#icon-'+icon);
    const failures=[...actions.values()].filter(action=>action.failed).length,notice=row.disclosure.querySelector('.action-failures');
    notice.hidden=!failures; notice.textContent=failures+' failed';
    const count=actions.size+parts.filter(part=>part.kind==='reasoning').length+[...actions.values()].reduce((count,action)=>count+action.changes.size,0);
    const single=count===1,wasSingle=row.disclosure.classList.contains('single-activity');
    // A single row opens its own details directly; adding another row retains any already-open details.
    if (single) row.disclosure.open=true;
    else if (wasSingle) row.disclosure.open=[...row.body.children].some(element=>element.open);
    row.disclosure.classList.toggle('single-activity',single);
    if (!row.disclosure.open) return;
    const present=new Set(),reasoning=new Set(); row.actionPosition=0;
    for (const part of parts)
    {
        if (part.kind==='reasoning')
        {
            let element=row.reasoning.get(part.id);
            if (!element)
            {
                element=document.createElement('details'); element.className='action-item';
                element.innerHTML='<summary><svg aria-hidden="true"><use href="#icon-lightbulb"/></svg><span></span><svg class="disclosure-arrow" aria-hidden="true"><use href="#icon-chevron"/></svg></summary><div class="answer progress-reasoning"></div>';
                enableDisclosureAnimation(element); row.reasoning.set(part.id,element);
            }
            const title=progressTitle([part]) || 'Thinking',label=element.querySelector('summary span');
            if (label.textContent!==title) label.textContent=title;
            label.classList.toggle('working',active && part===latest);
            renderAnswer(element.lastChild,part.text);
            const position=row.body.children[row.actionPosition++]; if (position!==element) row.body.insertBefore(element,position || null);
            reasoning.add(part.id); continue;
        }
        const action=actions.get(part.id);
        if (!action) continue;
        const generating=action.tool==='generate_image' && action.running;
        present.add(part.id); updateAction(row,part.id,action.icon,action.label,generating?'':action.output || JSON.stringify(part.data?.action || action.args,null,2),action.process);
        const item=row.actions.get(part.id);
        item.summary.classList.toggle('action-failed',action.failed); item.pre.classList.toggle('image-loading',generating);
        item.summary.querySelector('span').classList.toggle('working',active && action.running);
        if (generating) { item.pre.setAttribute('role','status'); item.pre.setAttribute('aria-label','Generating image'); }
        else { item.pre.removeAttribute('role'); item.pre.removeAttribute('aria-label'); }
        for (const change of action.changes.values())
        {
            const key=part.id+':'+change.path;
            present.add(key); updateAction(row,key,'edit',change.kind+' '+change.path,change.diff,null,change);
        }
    }
    for (const [key,item] of row.actions) if (!present.has(key)) { item.element.remove(); row.actions.delete(key); }
    for (const [key,element] of row.reasoning) if (!reasoning.has(key)) { element.remove(); row.reasoning.delete(key); }
}

// Keep one question visible at a time; choices advance immediately and the last answer submits the batch.
function createQuestionForm(request)
{
    const form=document.createElement('form'),answers=Object.create(null),drafts=Object.create(null);
    let index=0;
    form.className='question-request';
    // Rebuilding between questions preserves custom drafts and the answers already chosen.
    function showQuestion()
    {
        const question=request.questions[index],header=document.createElement('header'),back=document.createElement('button'),heading=document.createElement('span'),text=document.createElement('p'),options=document.createElement('div'),status=document.createElement('p');
        header.className='question-heading'; heading.textContent=request.questions.length>1?'Question '+(index+1)+' of '+request.questions.length:'Question';
        back.type='button'; back.className='icon-button question-back'; back.hidden=index===0; back.setAttribute('aria-label','Previous question'); setIcon(back,'chevron');
        back.addEventListener('click',()=>{ index--; showQuestion(); form.querySelector('.question-option,input,textarea').focus({preventScroll:true}); });
        header.append(heading,back); text.className='question-text'; text.textContent=question.question; options.className='question-options';
        for (const [number,option] of (question.options || []).entries())
        {
            const button=document.createElement('button'),badge=document.createElement('span'),label=document.createElement('span');
            button.type='submit'; button.name='answer'; button.value=option.label; button.className='question-option'; button.title=option.description;
            button.setAttribute('aria-pressed',String(answers[question.id]?.[0]===option.label));
            badge.className='question-number'; badge.textContent=number+1; badge.setAttribute('aria-hidden','true'); label.textContent=option.label;
            button.append(badge,label); options.append(button);
        }
        form.replaceChildren(header,text,options);
        if (question.isOther || !question.options?.length)
        {
            const row=document.createElement('div'),input=document.createElement(question.isSecret?'input':'textarea'),send=document.createElement('button');
            row.className='question-custom'; input.name='custom'; input.maxLength=8000; input.value=drafts[question.id] || '';
            if (question.isSecret) input.type='password';
            else input.rows=1;
            input.placeholder=question.options?.length?'Or write your own response':'Your response'; input.setAttribute('aria-label','Your response');
            send.name='custom-send'; send.className='primary'; send.textContent='Send'; send.disabled=!input.value.trim();
            input.addEventListener('input',()=>{ drafts[question.id]=input.value; send.disabled=!input.value.trim(); });
            input.addEventListener('keydown',event=>
            {
                if (event.key==='Enter' && !event.shiftKey && !event.isComposing) { event.preventDefault(); if (!send.disabled) form.requestSubmit(send); }
            });
            row.append(input,send); form.append(row);
        }
        status.className='question-status'; status.setAttribute('role','status'); form.append(status);
    }
    form.addEventListener('submit',async event=>
    {
        event.preventDefault();
        if (form.getAttribute('aria-busy')==='true') return;
        const question=request.questions[index],input=form.querySelector('[name=custom]'),status=form.querySelector('.question-status');
        const value=event.submitter?.name==='answer'?event.submitter.value:input?.value.trim();
        if (!value) { status.textContent='Please enter your answer'; return; }
        answers[question.id]=[value];
        if (index<request.questions.length-1)
        {
            index++; showQuestion(); focusPrompt(); return;
        }
        form.setAttribute('aria-busy','true'); status.textContent='';
        for (const control of form.elements) control.disabled=true;
        focusPrompt();
        try { await api('respond',{id:request.id,answer:{type:'questions',answers}}); }
        catch (error)
        {
            form.removeAttribute('aria-busy'); status.textContent=error.message;
            for (const control of form.elements) control.disabled=false;
            if (input) form.querySelector('[name=custom-send]').disabled=!input.value.trim();
        }
    });
    showQuestion(); return form;
}

// Reuse pending forms across streaming updates; questions dock above the compact composer.
function renderQuestions()
{
    const requests=view.interactions,present=new Set(requests.map(request=>request.id));
    for (const [id,form] of questionViews) if (!present.has(id)) { form.remove(); questionViews.delete(id); }
    for (const request of requests)
    {
        if (questionViews.has(request.id)) continue;
        if (request.type==='questions')
        {
            const form=createQuestionForm(request); questionViews.set(request.id,form); elements.questions.append(form); continue;
        }
        const form=document.createElement('form'),heading=document.createElement('h2'),reason=document.createElement('p'),details=document.createElement('details'),summary=document.createElement('summary'),pre=document.createElement('pre');
        form.className='native-request'; heading.textContent='Allow access on your computer?'; reason.textContent=request.action.reason;
        summary.textContent='Operation and exact arguments'; pre.textContent=JSON.stringify(request.action,null,2); details.append(summary,pre);
        const buttons=document.createElement('div'),submit=document.createElement('button'),decline=document.createElement('button'),status=document.createElement('p');
        buttons.className='button-row'; submit.className='primary'; submit.textContent='Allow'; decline.type='submit'; decline.name='decline'; decline.textContent='Decline'; buttons.append(submit,decline);
        form.append(heading,reason,details,buttons,status);
        form.addEventListener('submit',async event=>
        {
            event.preventDefault();
            for (const control of form.elements) control.disabled=true;
            focusPrompt();
            try { await api('respond',{id:request.id,answer:{type:'approval',approve:event.submitter?.name!=='decline'}}); }
            catch (error) { status.textContent=error.message; for (const control of form.elements) control.disabled=false; }
        });
        questionViews.set(request.id,form); elements.interactions.append(form);
    }
    const asking=!!elements.questions.childElementCount;
    elements.questions.hidden=!asking;
    elements['composer-wrap'].classList.toggle('answering-question',asking);
    elements.interactions.hidden=!elements.interactions.childElementCount;
}
