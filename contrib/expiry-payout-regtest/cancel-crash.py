"""SIGKILL before cancellation commit, then after cancellation before payout commit."""
import os,runpy
from pathlib import Path
os.environ['EXPIRY_CANCEL_CRASH']='1'
runpy.run_path(str(Path(__file__).with_name('mixed-outputs.py')))
