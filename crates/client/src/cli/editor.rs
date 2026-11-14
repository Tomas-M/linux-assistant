use crossterm::event::{KeyCode,KeyEvent,KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub(super) struct Editor { pub text:String,pub cursor:usize }
impl Editor {
    /// Start editing a queued draft at its end.
    pub fn new(text:&str)->Self { Self { text:text.into(),cursor:text.len() } }
    /// Bracketed paste preserves newlines but never injects terminal escape sequences.
    pub fn insert(&mut self,text:&str) {
        let text=text.replace("\r\n","\n").chars().filter(|c|!c.is_control() || matches!(*c,'\t'|'\n')).collect::<String>();
        self.text.insert_str(self.cursor,&text); self.cursor+=text.len();
    }
    /// Edit Unicode graphemes rather than deleting half a character or emoji.
    pub fn key(&mut self,key:KeyEvent) {
        let previous=self.text[..self.cursor].grapheme_indices(true).next_back().map_or(0,|(i,_)|i);
        let next=self.text[self.cursor..].graphemes(true).next().map_or(self.cursor,|g|self.cursor+g.len());
        match key.code {
            KeyCode::Backspace=> { self.text.replace_range(previous..self.cursor,""); self.cursor=previous; }
            KeyCode::Delete=> { self.text.replace_range(self.cursor..next,""); }
            KeyCode::Left=>self.cursor=previous,
            KeyCode::Right=>self.cursor=next,
            KeyCode::Home=>self.cursor=self.text[..self.cursor].rfind('\n').map_or(0,|i|i+1),
            KeyCode::End=>self.cursor=self.text[self.cursor..].find('\n').map_or(self.text.len(),|i|self.cursor+i),
            KeyCode::Up|KeyCode::Down=> {
                let line=self.text[..self.cursor].rfind('\n').map_or(0,|i|i+1);
                let column=self.text[line..self.cursor].graphemes(true).count();
                let target=if key.code==KeyCode::Up { line.checked_sub(1).map(|end|self.text[..end].rfind('\n').map_or(0,|i|i+1)) }
                    else { self.text[self.cursor..].find('\n').map(|i|self.cursor+i+1) };
                if let Some(start)=target {
                    let line=self.text[start..].split('\n').next().unwrap();
                    self.cursor=start+line.grapheme_indices(true).nth(column).map_or(line.len(),|(i,_)|i);
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL)=> { self.text.clear(); self.cursor=0; }
            KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL|KeyModifiers::ALT)=>self.insert(&c.to_string()),
            _=>{}
        }
    }
    /// Explicit wrapping lets the cursor follow pasted multiline and wide international text.
    pub fn wrapped(&self,width:u16,secret:bool)->(Vec<String>,(u16,u16)) {
        let width=usize::from(width.max(1));
        let mut lines=vec![String::new()]; let mut col=0; let mut cursor=(0,0);
        for (index,g) in self.text.grapheme_indices(true) {
            let g=if secret && g!="\n" { "*" } else if g=="\t" { "    " } else { g };
            let size=g.width();
            if g!="\n" && col+size>width { lines.push(String::new()); col=0; }
            if index==self.cursor { cursor=(col as u16,lines.len().saturating_sub(1) as u16); }
            if g=="\n" { lines.push(String::new()); col=0; }
            else { lines.last_mut().unwrap().push_str(g); col+=size; }
        }
        if col>=width { lines.push(String::new()); col=0; }
        if self.cursor==self.text.len() { cursor=(col as u16,lines.len().saturating_sub(1) as u16); }
        (lines,cursor)
    }
}

/// Keep model output, filenames and command logs inert in the terminal emulator.
pub(super) fn clean(text:&str)->String { text.chars().filter(|c|!c.is_control() || *c=='\n' || *c=='\t').collect::<String>().replace('\t',"    ") }
