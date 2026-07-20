import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { api } from '../api'

export function Login() {
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const navigate = useNavigate()

  async function submit(e: React.FormEvent) {
    e.preventDefault()
    setBusy(true)
    setError(null)
    try {
      await api.login(email, password)
      navigate('/')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'login failed')
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="login-wrap">
      <form className="card login" onSubmit={submit}>
        <h1>Chuggernaut</h1>
        <input
          type="email"
          placeholder="email"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          autoFocus
          required
        />
        <input
          type="password"
          placeholder="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          required
        />
        {error && <div className="error">{error}</div>}
        <button type="submit" disabled={busy}>
          {busy ? 'signing in…' : 'sign in'}
        </button>
      </form>
    </div>
  )
}
