import sys,re
# count lines inside top-level #[cfg(test)] items (brace-matched) vs production
for path in sys.argv[1:]:
    lines=open(path).read().split('\n')
    if lines and lines[-1]=='': lines=lines[:-1]
    test=0;i=0;n=len(lines)
    while i<n:
        if lines[i].strip()=='#[cfg(test)]' and not lines[i].startswith((' ','\t')):
            j=i+1;depth=0;started=False
            while j<n:
                s=re.sub(r'"(\\.|[^"\\])*"','""',lines[j]); s=re.sub(r"//.*","",s)
                depth+=s.count('{')-s.count('}')
                if '{' in s: started=True
                if started and depth<=0: break
                if not started and s.rstrip().endswith(';'): break
                j+=1
            test+=j-i+1;i=j+1
        else: i+=1
    print(f"{n:6d} prod={n-test:5d} test={test:5d}  {path}")
