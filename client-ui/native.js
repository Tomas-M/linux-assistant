'use strict';
const questionViews=new Map();

// Keep consecutive native tool activity in one disclosure; commentary, plans and user input break the group.
function renderProgress(container,parts,active=false,steers=[])
{
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
            const element=group.kind==='steer'?steeringRows.get(group.input.id):document.createElement(group.kind==='work'?'details':'div');
            let label,body=element;
            if (group.kind==='work')
            {
                element.className='tool-result';
                const summary=document.createElement('summary');
                summary.innerHTML='<svg aria-hidden="true"><use href="#icon-terminal"/></svg><span></span><svg class="disclosure-arrow" aria-hidden="true"><use href="#icon-chevron"/></svg>';
                label=summary.querySelector('span'); body=document.createElement('div'); element.append(summary,body);
            }
            else if (group.kind==='plan') element.className='live-plan';
            else if (group.kind!=='steer') element.className='answer progress-commentary';
            row={element,label,body,actions:new Map(),reasoning:new Map()}; rows.set(group.key,row);
        }
        if (group.kind==='work') renderWork(row,group.parts,active && index===groups.length-1);
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

// Polling reuses its session row, while verified changes and reasoning retain their own nested disclosures.
function renderWork(row,parts,active)
{
    const actions=parts.filter(part=>part.kind!=='reasoning'), changes=actions.flatMap(part=>view.tool_results[part.id]?.changes || []);
    let label=actions.at(-1)?.text || 'Thinking…';
    if (!active && actions.length)
    {
        const commands=actions.some(part=>['local_exec_command','local_write_stdin','local_stop_process'].includes(part.data?.tool));
        const files=actions.some(part=>['local_read_file','local_list_directory','local_search_files','local_get_metadata'].includes(part.data?.tool));
        const images=actions.some(part=>part.data?.tool==='local_view_image');
        const generated=actions.some(part=>part.data?.tool==='generate_image');
        const modified=actions.some(part=>['local_create_directory','local_copy_path','local_move_path','local_remove_path','local_create_symlink','local_set_permissions'].includes(part.data?.tool) && view.tool_results[part.id]?.success);
        let memoryLabel='';
        if (actions.some(part=>part.data?.tool==='memory_save')) memoryLabel='updated memories';
        else if (actions.some(part=>part.data?.tool==='memory_read')) memoryLabel='read memories';
        label=[changes.length?'Edited files':modified?'Updated files':files?'Read files':'',images?'viewed images':'',generated?'generated images':'',commands?'ran commands':'',actions.some(part=>part.kind==='web_search')?'searched the web':'',memoryLabel].filter(Boolean).join(', ');
        label=label?label[0].toUpperCase()+label.slice(1):'Completed actions';
    }
    const title=progressTitle(parts) || label;
    if (row.label.textContent!==title) row.label.textContent=title;
    row.label.classList.toggle('working',active);
    const icon=row.element.querySelector('summary>svg:first-child'); icon.toggleAttribute('hidden',!actions.length);
    const hasCommands=actions.some(part=>['local_exec_command','local_write_stdin','local_stop_process'].includes(part.data?.tool));
    let groupIcon='list-ul';
    if (changes.length) groupIcon='edit';
    else if (hasCommands) groupIcon='terminal';
    else if (actions.some(part=>part.data?.tool==='generate_image')) groupIcon='image';
    else if (actions.some(part=>part.kind==='web_search')) groupIcon='external';
    else if (actions.some(part=>['memory_read','memory_save'].includes(part.data?.tool))) groupIcon='layer-group';
    const use=icon.querySelector('use');
    if (use.getAttribute('href')!=='#icon-'+groupIcon) use.setAttribute('href','#icon-'+groupIcon);
    const sessions=new Map(), present=new Set(), reasoning=new Set();
    for (const part of actions)
    {
        const result=view.tool_results[part.id], session=result?.session_id || part.data?.arguments?.session_id || part.id;
        if (!sessions.has(session)) sessions.set(session,{first:part.id,parts:[]});
        sessions.get(session).parts.push(part);
    }
    row.actionPosition=0;
    for (const part of parts)
    {
        if (part.kind==='reasoning')
        {
            let element=row.reasoning.get(part.id);
            if (!element)
            {
                element=document.createElement('details'); element.className='past-progress';
                element.innerHTML='<summary><span></span><svg aria-hidden="true"><use href="#icon-chevron"/></svg></summary><div class="answer progress-body"></div>';
                row.reasoning.set(part.id,element);
            }
            const title=progressTitle([part]) || 'Thinking', label=element.querySelector('summary span');
            if (label.textContent!==title) label.textContent=title;
            renderAnswer(element.lastChild,part.text);
            const position=row.body.children[row.actionPosition++]; if (position!==element) row.body.insertBefore(element,position || null);
            reasoning.add(part.id); continue;
        }
        const result=view.tool_results[part.id], id=result?.session_id || part.data?.arguments?.session_id || part.id, session=sessions.get(id);
        if (session.first!==part.id) continue;
        const last=session.parts.at(-1), latest=view.tool_results[last.id], args=part.data?.arguments || {};
        const output=session.parts.map(item=>view.tool_results[item.id]?.output || '').filter(Boolean).join('\n\n');
        const command=part.data?.tool==='local_exec_command';
        const running=latest?.running ?? !last.completed;
        const caption=command?(running?'Running ':'Ran ')+args.cmd:part.text;
        let toolIcon=part.kind==='web_search'?'external':command || args.session_id?'terminal':(result?.changes || []).length?'edit':'list-ul';
        if (['memory_read','memory_save'].includes(part.data?.tool)) toolIcon='layer-group';
        if (part.data?.tool==='generate_image') toolIcon='image';
        // The first native item stays stable when the local session ID arrives later.
        const key=session.first, process=result?.session_id || args.session_id;
        const generating=part.data?.tool==='generate_image' && running;
        present.add(key); updateAction(row,key,toolIcon,caption,generating?'':output || JSON.stringify(part.data?.action || args,null,2),process);
        const preview=row.actions.get(key).pre;
        preview.classList.toggle('image-loading',generating);
        if (generating) { preview.setAttribute('role','status'); preview.setAttribute('aria-label','Generating image'); }
        else { preview.removeAttribute('role'); preview.removeAttribute('aria-label'); }
        const changes=new Map();
        for (const item of session.parts)
            for (const change of view.tool_results[item.id]?.changes || []) changes.set(change.path,change);
        for (const change of changes.values())
        {
            const key=session.first+':'+change.path;
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
