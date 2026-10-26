# loggen.pl <lines_per_burst> <bursts_per_sec>: log-like ASCII lines (~110 chars) at a fixed rate
use Time::HiRes qw(sleep time); $|=1;
my ($n,$hz)=@ARGV; my $i=0; my @w=qw(kernel launchd WindowServer mDNSResponder trustd cfprefsd runningboardd);
while(1){ my $t=time; my $s='';
  for(1..$n){ $i++; $s.=sprintf("2026-10-24 01:%02d:%06.3f Df %s[%d:%x] (CoreFoundation) request %d completed status=ok bytes=%d elapsed=%.3fms\n", ($i/6000)%60, ($i/100)%60, $w[$i%7], 100+$i%900, $i*7, $i, ($i*37)%65536, ($i%1000)/7); }
  print $s; my $d=1/$hz-(time-$t); sleep($d) if $d>0; }
