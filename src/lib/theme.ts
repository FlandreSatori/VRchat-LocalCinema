const THEME_KEY = 'localcinema-theme'

export function isDark(): boolean {
  return document.documentElement.classList.contains('dark')
}

export function initTheme() {
  const saved = localStorage.getItem(THEME_KEY)
  const dark = saved ? saved === 'dark' : true
  document.documentElement.classList.toggle('dark', dark)
}

export function toggleTheme(): boolean {
  const dark = !isDark()
  document.documentElement.classList.toggle('dark', dark)
  localStorage.setItem(THEME_KEY, dark ? 'dark' : 'light')
  return dark
}
