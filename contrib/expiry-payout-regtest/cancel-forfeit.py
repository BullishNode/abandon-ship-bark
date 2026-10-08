"""A genuine, verified forfeit that cached an exchange loses to its cancellation."""
import os,runpy
from pathlib import Path
os.environ['EXPIRY_CANCEL_FORFEIT']='1'
runpy.run_path(str(Path(__file__).with_name('mixed-outputs.py')))
